// macOS banners for Filo's replies (UNUserNotificationCenter), with sound, one
// per chat while the user is in another app or Filo is closed: a newer one
// replaces the chat's banner, and reading the chat removes it. The system ties
// notifications to an app bundle, so they only work in Filo.app
// (scripts/app.sh); the bare binary skips them.
//
// macOS only notifies for apps signed with an Apple-issued certificate (Apple
// Development for local builds, Developer ID for releases; README). Ad-hoc and
// self-signed builds are refused without a prompt ("Notifications are not
// allowed for this application") and Filo never shows in System Settings;
// Settings › Notifications says so.
//
// Nothing is asked at launch: the first time Filo has something to show (a reply arriving while Filo is in the
// background, a reminder, routine or check-in made or coming due) the app explains why in a card, then asks macOS
// (`needed`, in_app_notice.rs `ask_for_banners`). macOS asks only once; an answer stands until changed in System
// Settings.
//
// A new build (updater.rs) gets one silent banner: ready, with a Restart button, or, with automatic downloads off,
// available, with an Update button (`show_update`).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread as _, class, define_class, msg_send};
use objc2_foundation::{NSArray, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse, UNNotificationSettings,
    UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

const CHAT_PREFIX: &str = "filo-chat-";
/// An update's banner, `filo-update-<build>`, in the category with its button.
const UPDATE_PREFIX: &str = "filo-update-";
const UPDATE_THREAD: &str = "filo-update";

/// Chats whose banner was clicked, for the app to open.
static CLICKS: OnceLock<UnboundedSender<String>> = OnceLock::new();
/// Moments Filo first needs its banners, for the app to ask.
static NEEDS: OnceLock<UnboundedSender<Need>> = OnceLock::new();
/// Buttons clicked on an update's banner, for the updater.
static UPDATE_ACTIONS: OnceLock<UnboundedSender<UpdateAction>> = OnceLock::new();

/// The button on an update's banner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAction {
    /// Downloaded and ready: Restart puts it in place.
    Restart,
    /// Available (automatic downloads off): Update downloads, installs and restarts, as the sidebar's Update button does.
    Update,
}

impl UpdateAction {
    const ALL: [UpdateAction; 2] = [UpdateAction::Restart, UpdateAction::Update];

    fn id(self) -> &'static str {
        match self {
            UpdateAction::Restart => "restart",
            UpdateAction::Update => "update",
        }
    }

    fn title(self) -> &'static str {
        match self {
            UpdateAction::Restart => "Restart",
            UpdateAction::Update => "Update",
        }
    }

    fn category(self) -> String {
        format!("filo-update-{}", self.id())
    }
}
/// macOS refused this build outright (its signature isn't Apple-issued).
static REFUSED: AtomicBool = AtomicBool::new(false);
/// macOS said yes: its banners show, with their sound.
static GRANTED: AtomicBool = AtomicBool::new(false);

/// Whether macOS shows Filo's banners (and plays their sound).
pub fn banners_on() -> bool {
    GRANTED.load(Ordering::Relaxed)
}
/// `UNErrorCodeNotificationsNotAllowed`.
const NOT_ALLOWED: isize = 1;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "FiloNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// Banners show while Filo is in front too: a reminder coming due, a note after the Mac slept. Replies get none
        /// then (the card in the window shows them, unseen.rs), nor do routines' and check-ins' (scheduled.rs `announce`).
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            handler: &block2::DynBlock<dyn Fn()>,
        ) {
            let id = response.notification().request().identifier().to_string();
            if let (Some(session), Some(clicks)) = (id.strip_prefix(CHAT_PREFIX), CLICKS.get()) {
                let _ = clicks.unbounded_send(session.to_string());
            }
            let action = response.actionIdentifier().to_string();
            if let Some(action) = UpdateAction::ALL
                .into_iter()
                .find(|known| known.id() == action)
                .filter(|_| id.starts_with(UPDATE_PREFIX))
            {
                update_action(action);
            }
            handler.call(());
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

fn center() -> Option<Retained<UNUserNotificationCenter>> {
    crate::recorder::in_app_bundle().then(UNUserNotificationCenter::currentNotificationCenter)
}

/// Why Filo wants to show banners, for the note before macOS asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// A reply came in while Filo wasn't in front.
    Reply,
    /// A reminder, routine or check-in was made or came due.
    Scheduled,
}

/// Takes banner clicks and learns what macOS allows, without asking; returns the clicked chats and the moments Filo
/// first needs its banners (`needed`). None outside Filo.app or when already set up.
pub fn start() -> Option<(UnboundedReceiver<String>, UnboundedReceiver<Need>)> {
    let center = center()?;
    let (tx, clicks) = unbounded();
    CLICKS.set(tx).ok()?;
    let (tx, needs) = unbounded();
    NEEDS.set(tx).ok()?;
    let delegate = Delegate::new();
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // The center holds its delegate weakly; it lives as long as the app.
    std::mem::forget(delegate);
    set_categories(&center);
    // Whether banners are on (`banners_on`) for users who answered before.
    drop(permission());
    Some((clicks, needs))
}

/// Filo has something it would show as a banner; the app asks for them if macOS hasn't yet. From any thread; does
/// nothing in the background helper, which has no window to explain from.
pub fn needed(need: Need) {
    if let Some(needs) = NEEDS.get() {
        let _ = needs.unbounded_send(need);
    }
}

/// Shows (or replaces) the chat's banner.
pub fn show_chat(session_id: &str, title: &str, subtitle: Option<&str>, body: &str) {
    let Some(center) = center() else { return };
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    if let Some(subtitle) = subtitle {
        content.setSubtitle(&NSString::from_str(subtitle));
    }
    content.setBody(&NSString::from_str(body));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    content.setThreadIdentifier(&NSString::from_str(session_id));
    let id = NSString::from_str(&format!("{CHAT_PREFIX}{session_id}"));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
    center.addNotificationRequest_withCompletionHandler(&request, None);
}

/// A banner from scheduled work: a reminder coming due, what a routine or check-in sent. Also posted by the background
/// helper (`filo --scheduler`, launch_agent.rs) while the app is closed; it runs Filo.app's own binary, so its banners
/// are Filo's too.
pub fn show_scheduled(id: &str, title: &str, body: &str) {
    let Some(center) = center() else { return };
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(id), &content, None);
    center.addNotificationRequest_withCompletionHandler(&request, None);
}

/// The buttons on an update's banner: a category each.
fn set_categories(center: &UNUserNotificationCenter) {
    let categories: Vec<_> = UpdateAction::ALL
        .into_iter()
        .map(|action| {
            let button = UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str(action.id()),
                &NSString::from_str(action.title()),
                UNNotificationActionOptions::empty(),
            );
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(&action.category()),
                &NSArray::from_retained_slice(&[button]),
                &NSArray::new(),
                UNNotificationCategoryOptions::empty(),
            )
        })
        .collect();
    let categories = NSArray::from_retained_slice(&categories);
    // SAFETY: an NSSet of the categories, handed to the center, which copies it.
    unsafe {
        let set: *mut objc2::runtime::AnyObject = msg_send![class!(NSSet), setWithArray: &*categories];
        let _: () = msg_send![center, setNotificationCategories: set];
    }
}

/// The buttons clicked on an update's banner (`show_update`), for the updater; None outside Filo.app or once taken.
pub fn update_actions() -> Option<UnboundedReceiver<UpdateAction>> {
    center()?;
    let (tx, actions) = unbounded();
    UPDATE_ACTIONS.set(tx).ok()?;
    Some(actions)
}

/// What a click on an update banner's button does: hands it to the updater. Also how a `dev` check presses one.
pub fn update_action(action: UpdateAction) {
    if let Some(actions) = UPDATE_ACTIONS.get() {
        let _ = actions.unbounded_send(action);
    }
}

/// A new build (updater.rs): a silent banner, `filo-update-<build>`, with `action`'s button.
pub fn show_update(build: u64, title: &str, body: &str, action: UpdateAction) {
    let Some(center) = center() else {
        eprintln!("update: no banner (not in Filo.app)");
        return;
    };
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    content.setCategoryIdentifier(&NSString::from_str(&action.category()));
    content.setThreadIdentifier(&NSString::from_str(UPDATE_THREAD));
    let id = NSString::from_str(&format!("{UPDATE_PREFIX}{build}"));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
    let handler = block2::RcBlock::new(move |error: *mut NSError| match unsafe { error.as_ref() } {
        Some(error) => {
            eprintln!("update: macOS didn’t show the banner for build {build}: {}", error.localizedDescription())
        }
        None => eprintln!("update: banner for build {build} handed to macOS"),
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&handler));
}

/// Takes down the banners of chats that were read.
pub fn remove_chats(session_ids: &[String]) {
    if session_ids.is_empty() {
        return;
    }
    let Some(center) = center() else { return };
    let ids: Vec<Retained<NSString>> =
        session_ids.iter().map(|id| NSString::from_str(&format!("{CHAT_PREFIX}{id}"))).collect();
    center.removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(&ids));
}

/// What macOS lets Filo do (System Settings › Notifications › Filo).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    /// Running as the bare binary: macOS only notifies for an app bundle.
    NotAnApp,
    /// macOS won't notify for this build at all: it isn't signed with an Apple-issued certificate.
    NotSigned,
    NotAsked,
    Denied,
    Allowed {
        sound: bool,
    },
}

/// Asks macOS what Filo may do; resolves on whatever queue macOS answers on.
pub fn permission() -> futures::channel::oneshot::Receiver<Permission> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let Some(center) = center() else {
        let _ = tx.send(Permission::NotAnApp);
        return rx;
    };
    if REFUSED.load(Ordering::Relaxed) {
        let _ = tx.send(Permission::NotSigned);
        return rx;
    }
    let tx = std::sync::Mutex::new(Some(tx));
    let handler = block2::RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
        let settings = unsafe { settings.as_ref() };
        let permission = match settings.authorizationStatus() {
            UNAuthorizationStatus::NotDetermined => Permission::NotAsked,
            UNAuthorizationStatus::Denied => Permission::Denied,
            // `UNNotificationSetting::Enabled` is 2.
            _ => Permission::Allowed { sound: settings.soundSetting().0 == 2 },
        };
        GRANTED.store(matches!(permission, Permission::Allowed { .. }), Ordering::Relaxed);
        if let Some(tx) = tx.lock().ok().and_then(|mut tx| tx.take()) {
            let _ = tx.send(permission);
        }
    });
    center.getNotificationSettingsWithCompletionHandler(&handler);
    rx
}

/// Asks to notify; macOS shows its prompt only the first time. Only when Filo needs it (`needed`) or the user asks.
pub fn request() {
    if crate::test_run::refuse("notifications prompt") {
        return;
    }
    let Some(center) = center() else { return };
    let handler = block2::RcBlock::new(|granted: Bool, error: *mut NSError| {
        GRANTED.store(granted.as_bool(), Ordering::Relaxed);
        if unsafe { error.as_ref() }.is_some_and(|error| error.code() == NOT_ALLOWED) {
            REFUSED.store(true, Ordering::Relaxed);
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge,
        &handler,
    );
}

/// System Settings › Notifications, where Filo's banners and sound are switched on.
pub fn open_settings() {
    let _ = open::that_detached("x-apple.systempreferences:com.apple.Notifications-Settings.extension");
}
