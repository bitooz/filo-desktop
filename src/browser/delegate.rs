// The browser's WebKit UI delegate (browser.rs). wry's own (`WryWebViewUIDelegate`) has no alert, confirm or prompt,
// so WebKit answered them itself (nothing shown, confirm false, prompt null), runs a modal open panel on the user's
// screen when a page asks for a file, and leaves a page's window.close() undone. Each tab's delegate is moved to a
// subclass of Filo's (its class changed in place, as KVO does, then set on the web view again so WebKit reads its
// methods anew) that hands those to browser.rs: a dialog waits for its answer (browser_dialog, or the bar in the browser
// window), a file chooser takes the files an agent gave (browser_upload) or, for the user, the open panel as before,
// and a page that closes itself closes its tab. Other web views of the app keep wry's delegate.
//
// The navigation delegate gets a subclass the same way: a response the server says is an attachment
// (`Content-Disposition: attachment`, an export or a report link) downloads, as in a browser; wry's downloads only
// what WebKit can't show (a PDF sent as an attachment opened as a page).

use std::cell::RefCell;
use std::collections::HashMap;

use block2::{Block, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::NSString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Alert,
    Confirm,
    Prompt,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Alert => "alert",
            Kind::Confirm => "confirm",
            Kind::Prompt => "prompt",
        }
    }

    /// "an alert", "a confirm", "a prompt".
    pub fn a(self) -> &'static str {
        match self {
            Kind::Alert => "an alert",
            Kind::Confirm => "a confirm",
            Kind::Prompt => "a prompt",
        }
    }
}

enum Reply {
    Alert(RcBlock<dyn Fn()>),
    Confirm(RcBlock<dyn Fn(Bool)>),
    Prompt(RcBlock<dyn Fn(*mut AnyObject)>),
    /// A page in Chromium's (browser/chromium.rs): OK or Cancel, and a prompt's text.
    #[cfg(feature = "dev")]
    Chromium(Box<dyn FnOnce(bool, String)>),
    Done,
}

/// A page's alert, confirm or prompt, waiting: the page's script stops until it's answered. Dropped unanswered, it's
/// dismissed (Cancel), as WebKit requires every dialog to be answered.
pub struct Dialog {
    pub kind: Kind,
    pub message: String,
    /// A prompt's text as the page filled it in.
    pub default: String,
    reply: Reply,
}

impl Dialog {
    /// OK (`accept`) or Cancel; a prompt's OK gives `text`, else its default.
    pub fn answer(mut self, accept: bool, text: Option<&str>) {
        self.finish(accept, text);
    }

    fn finish(&mut self, accept: bool, text: Option<&str>) {
        match std::mem::replace(&mut self.reply, Reply::Done) {
            Reply::Alert(done) => done.call(()),
            Reply::Confirm(done) => done.call((Bool::new(accept),)),
            Reply::Prompt(done) => {
                if accept {
                    let text = NSString::from_str(text.unwrap_or(&self.default));
                    done.call((Retained::as_ptr(&text) as *mut AnyObject,));
                } else {
                    done.call((std::ptr::null_mut(),));
                }
            }
            #[cfg(feature = "dev")]
            Reply::Chromium(done) => done(accept, text.unwrap_or(&self.default).to_string()),
            Reply::Done => {}
        }
    }

    /// A dialog of a page in Chromium, answered through `done`.
    #[cfg(feature = "dev")]
    pub fn chromium(kind: Kind, message: String, default: String, done: impl FnOnce(bool, String) + 'static) -> Dialog {
        Dialog { kind, message, default, reply: Reply::Chromium(Box::new(done)) }
    }
}

impl Drop for Dialog {
    fn drop(&mut self) {
        self.finish(false, None);
    }
}

/// What a page's file chooser gets.
pub enum Chooser {
    Files(Vec<std::path::PathBuf>),
    /// No file (an agent's page that asked when no files were given).
    Cancel,
    /// The open panel, for the user (wry's own).
    User,
}

fn string(object: *mut AnyObject) -> String {
    if object.is_null() {
        return String::new();
    }
    // SAFETY: an NSString, read on the main thread.
    unsafe {
        let text: *const std::ffi::c_char = msg_send![object, UTF8String];
        if text.is_null() { String::new() } else { std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned() }
    }
}

extern "C-unwind" fn alert(
    _: &AnyObject,
    _: Sel,
    web: *mut AnyObject,
    message: *mut AnyObject,
    _: *mut AnyObject,
    done: &Block<dyn Fn()>,
) {
    super::on_dialog(
        web,
        Dialog {
            kind: Kind::Alert,
            message: string(message),
            default: String::new(),
            reply: Reply::Alert(done.copy()),
        },
    );
}

extern "C-unwind" fn confirm(
    _: &AnyObject,
    _: Sel,
    web: *mut AnyObject,
    message: *mut AnyObject,
    _: *mut AnyObject,
    done: &Block<dyn Fn(Bool)>,
) {
    super::on_dialog(
        web,
        Dialog {
            kind: Kind::Confirm,
            message: string(message),
            default: String::new(),
            reply: Reply::Confirm(done.copy()),
        },
    );
}

extern "C-unwind" fn prompt(
    _: &AnyObject,
    _: Sel,
    web: *mut AnyObject,
    message: *mut AnyObject,
    default: *mut AnyObject,
    _: *mut AnyObject,
    done: &Block<dyn Fn(*mut AnyObject)>,
) {
    super::on_dialog(
        web,
        Dialog {
            kind: Kind::Prompt,
            message: string(message),
            default: string(default),
            reply: Reply::Prompt(done.copy()),
        },
    );
}

extern "C-unwind" fn open_panel(
    this: &AnyObject,
    _: Sel,
    web: *mut AnyObject,
    parameters: *mut AnyObject,
    frame: *mut AnyObject,
    done: &Block<dyn Fn(*const AnyObject)>,
) {
    // SAFETY: WKOpenPanelParameters, read on the main thread.
    let (multiple, directories) = unsafe {
        let multiple: bool = msg_send![parameters, allowsMultipleSelection];
        let directories: bool = msg_send![parameters, allowsDirectories];
        (multiple, directories)
    };
    match super::on_file_chooser(web, multiple, directories) {
        Chooser::Files(paths) => {
            // SAFETY: Foundation on the main thread; the array lives until the call returns (WebKit copies it).
            unsafe {
                let urls: Retained<AnyObject> = msg_send![class!(NSMutableArray), new];
                for path in paths {
                    let path = NSString::from_str(&path.to_string_lossy());
                    let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: &*path];
                    let _: () = msg_send![&*urls, addObject: url];
                }
                done.call((Retained::as_ptr(&urls),));
            }
        }
        Chooser::Cancel => done.call((std::ptr::null(),)),
        Chooser::User => match ORIGINAL
            .with(|original| original.borrow().get(&(this.class() as *const AnyClass as usize)).copied())
        {
            // SAFETY: wry's own implementation, with the arguments WebKit gave.
            Some(original) => unsafe {
                let original = &*(original as *const AnyClass);
                let _: () = msg_send![super(this, original), webView: web, runOpenPanelWithParameters: parameters, initiatedByFrame: frame, completionHandler: done];
            },
            None => done.call((std::ptr::null(),)),
        },
    }
}

extern "C-unwind" fn closed(_: &AnyObject, _: Sel, web: *mut AnyObject) {
    super::on_page_closed(web);
}

/// Whether a WKNavigationResponse's server says it's an attachment.
fn attachment(response: *mut AnyObject) -> bool {
    // SAFETY: WebKit and Foundation read-only queries on the main thread.
    unsafe {
        let response: *mut AnyObject = msg_send![response, response];
        if response.is_null() {
            return false;
        }
        let http: bool = msg_send![response, isKindOfClass: class!(NSHTTPURLResponse)];
        if !http {
            return false;
        }
        let header = NSString::from_str("Content-Disposition");
        let value: *mut AnyObject = msg_send![response, valueForHTTPHeaderField: &*header];
        string(value).trim_start().to_ascii_lowercase().starts_with("attachment")
    }
}

extern "C-unwind" fn navigation_response(
    this: &AnyObject,
    _: Sel,
    web: *mut AnyObject,
    response: *mut AnyObject,
    done: &Block<dyn Fn(isize)>,
) {
    if attachment(response) {
        // WKNavigationResponsePolicyDownload: wry's download delegate takes it from there.
        done.call((2,));
        return;
    }
    match ORIGINAL.with(|original| original.borrow().get(&(this.class() as *const AnyClass as usize)).copied()) {
        // SAFETY: wry's own implementation, with the arguments WebKit gave.
        Some(original) => unsafe {
            let original = &*(original as *const AnyClass);
            let _: () = msg_send![super(this, original), webView: web, decidePolicyForNavigationResponse: response, decisionHandler: done];
        },
        // WKNavigationResponsePolicyAllow
        None => done.call((1,)),
    }
}

thread_local! {
    /// Filo's subclass of each delegate class, and the class each subclass was made from, by address.
    static SUBCLASS: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
    static ORIGINAL: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
}

fn subclass_of(class: &AnyClass, methods: fn(&mut ClassBuilder)) -> Option<&'static AnyClass> {
    let key = class as *const AnyClass as usize;
    if ORIGINAL.with(|original| original.borrow().contains_key(&key)) {
        // SAFETY: a registered class lives as long as the process.
        return Some(unsafe { &*(key as *const AnyClass) });
    }
    if let Some(sub) = SUBCLASS.with(|subclass| subclass.borrow().get(&key).copied()) {
        // SAFETY: as above.
        return Some(unsafe { &*(sub as *const AnyClass) });
    }
    let name = std::ffi::CString::new(format!("FiloBrowser_{}", class.name().to_string_lossy())).ok()?;
    let sub = match AnyClass::get(&name) {
        Some(sub) => sub,
        None => {
            let mut builder = ClassBuilder::new(&name, class)?;
            methods(&mut builder);
            builder.register()
        }
    };
    let sub_key = sub as *const AnyClass as usize;
    SUBCLASS.with(|subclass| subclass.borrow_mut().insert(key, sub_key));
    ORIGINAL.with(|original| original.borrow_mut().insert(sub_key, key));
    Some(sub)
}

fn ui_methods(builder: &mut ClassBuilder) {
    // SAFETY: each method's signature matches WKUIDelegate's (blocks are passed as object pointers).
    unsafe {
        builder.add_method(
            sel!(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:),
            alert as extern "C-unwind" fn(_, _, _, _, _, _),
        );
        builder.add_method(
            sel!(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:),
            confirm as extern "C-unwind" fn(_, _, _, _, _, _),
        );
        builder.add_method(
            sel!(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:),
            prompt as extern "C-unwind" fn(_, _, _, _, _, _, _),
        );
        builder.add_method(
            sel!(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:),
            open_panel as extern "C-unwind" fn(_, _, _, _, _, _),
        );
        builder.add_method(sel!(webViewDidClose:), closed as extern "C-unwind" fn(_, _, _));
    }
}

fn navigation_methods(builder: &mut ClassBuilder) {
    // SAFETY: the signature matches WKNavigationDelegate's (the block is passed as an object pointer).
    unsafe {
        builder.add_method(
            sel!(webView:decidePolicyForNavigationResponse:decisionHandler:),
            navigation_response as extern "C-unwind" fn(_, _, _, _, _),
        );
    }
}

/// Moves a tab's web view's UI delegate to Filo's subclass, and sets it again so WebKit reads its methods; and its
/// navigation delegate (whose one method wry has already, so WebKit calls it as it is).
pub fn adopt(view: &AnyObject) {
    // SAFETY: AppKit and WebKit on the main thread; the delegates are kept by wry's web view, and their new classes
    // are subclasses with no state of their own.
    unsafe {
        let delegate: *mut AnyObject = msg_send![view, UIDelegate];
        if let Some(sub) = (!delegate.is_null()).then(|| subclass_of((*delegate).class(), ui_methods)).flatten() {
            objc2::ffi::object_setClass(delegate, sub);
            let _: () = msg_send![view, setUIDelegate: std::ptr::null_mut::<AnyObject>()];
            let _: () = msg_send![view, setUIDelegate: delegate];
        }
        let navigation: *mut AnyObject = msg_send![view, navigationDelegate];
        if let Some(sub) =
            (!navigation.is_null()).then(|| subclass_of((*navigation).class(), navigation_methods)).flatten()
        {
            objc2::ffi::object_setClass(navigation, sub);
        }
    }
}
