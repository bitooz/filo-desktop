// Filo Chromium: the test engine for Filo's browser (`dev` builds, FILO_BROWSER_ENGINE=chromium; src/browser/chromium.rs).
// Chromium through CEF in windowless mode: each tab is a browser with no window, drawn into a frame in shared memory
// that Filo shows in its browser window. Filo starts this app and talks to it over its standard input and output, one
// JSON line each way:
//
//   in:  {"id":1,"op":"create","tab":"t1","url":"…","width":1280,"height":800,"scale":2}
//        {"id":2,"op":"cdp","tab":"t1","method":"Runtime.evaluate","params":{…}}   (DevTools, answered with its result)
//        {"op":"load","tab":"t1","url":"…"}  {"op":"go","tab":"t1","how":"back"}  {"op":"close","tab":"t1"}
//        {"op":"resize","tab":"t1","width":…,"height":…,"scale":2}
//        {"op":"dialog","tab":"t1","accept":true,"text":"…"}  {"op":"files","tab":"t1","paths":[…]}  (none: cancel)
//        {"op":"download","tab":"t1","item":3,"path":"…"}  ("": cancel)  {"op":"rename","tab":"p1","to":"t7"}
//   out: {"id":1,"ok":true}  {"id":2,"ok":true,"result":{…}}  {"id":2,"ok":false,"error":"…"}
//        {"ev":"state","tab":"t1","title":"…","url":"…","loading":false,"back":true,"forward":false}
//        {"ev":"frame","tab":"t1","shm":"/filo-cr-…","width":2560,"height":1600,"seq":7}
//        {"ev":"popup","tab":"p1","opener":"t1","url":"…"}  {"ev":"closed","tab":"p1"}  {"ev":"cursor","tab":"t1","type":2}
//        {"ev":"dialog","tab":"t1","kind":"confirm","message":"…","default":""}  {"ev":"files","tab":"t1","multiple":false}
//        {"ev":"download","tab":"t1","item":3,"url":"…","name":"…"}  {"ev":"downloaded","tab":"t1","item":3,"url":"…","ok":true}
//
// Agents' clicks and keys are DevTools input (Input.dispatchMouseEvent and the like), so pages get trusted events.
// The app quits when Filo closes its standard input. Built by chromium/build.sh.

#import <Cocoa/Cocoa.h>

#include <fcntl.h>
#include <sys/mman.h>
#include <unistd.h>

#include <cstdio>
#include <iostream>
#include <map>
#include <mutex>
#include <string>
#include <set>
#include <thread>
#include <vector>

#include "include/base/cef_callback.h"
#include "include/cef_app.h"
#include "include/cef_application_mac.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/cef_cookie.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_dialog_handler.h"
#include "include/cef_download_handler.h"
#include "include/cef_jsdialog_handler.h"
#include "include/cef_parser.h"
#include "include/cef_version.h"
#include "include/wrapper/cef_closure_task.h"
#include "include/wrapper/cef_helpers.h"
#include "include/wrapper/cef_library_loader.h"

namespace {

// The protocol's output: Filo's pipe, kept apart from standard output, which goes to standard error (Chromium and the
// system libraries print there).
int g_out = -1;
std::mutex g_out_lock;

void Send(const std::string& line) {
  std::lock_guard<std::mutex> lock(g_out_lock);
  std::string text = line + "\n";
  const char* at = text.data();
  size_t left = text.size();
  while (left > 0) {
    ssize_t written = write(g_out, at, left);
    if (written <= 0) return;
    at += written;
    left -= written;
  }
}

std::string Quote(const std::string& text) {
  std::string out = "\"";
  for (unsigned char c : text) {
    switch (c) {
      case '"': out += "\\\""; break;
      case '\\': out += "\\\\"; break;
      case '\n': out += "\\n"; break;
      case '\r': out += "\\r"; break;
      case '\t': out += "\\t"; break;
      default:
        if (c < 0x20) {
          char buffer[8];
          snprintf(buffer, sizeof buffer, "\\u%04x", c);
          out += buffer;
        } else {
          out += static_cast<char>(c);
        }
    }
  }
  return out + "\"";
}

void Answer(int id, bool ok, const std::string& error = std::string()) {
  if (id <= 0) return;
  if (ok) {
    Send("{\"id\":" + std::to_string(id) + ",\"ok\":true}");
  } else {
    Send("{\"id\":" + std::to_string(id) + ",\"ok\":false,\"error\":" + Quote(error) + "}");
  }
}

class Tab;
std::map<std::string, CefRefPtr<Tab>> g_tabs;
int g_popups = 0;
bool g_quitting = false;

void QuitWhenClosed();

// One tab: a windowless browser, its frame in shared memory, and its DevTools calls waiting for answers.
class Tab : public CefClient,
            public CefRenderHandler,
            public CefDisplayHandler,
            public CefLoadHandler,
            public CefLifeSpanHandler,
            public CefRequestHandler,
            public CefJSDialogHandler,
            public CefDialogHandler,
            public CefDownloadHandler,
            public CefDevToolsMessageObserver {
 public:
  Tab(std::string key, int width, int height, float scale, std::string opener)
      : key_(std::move(key)), width_(width), height_(height), scale_(scale), opener_(std::move(opener)) {}

  ~Tab() override { DropFrame(); }

  CefRefPtr<CefRenderHandler> GetRenderHandler() override { return this; }
  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }
  CefRefPtr<CefJSDialogHandler> GetJSDialogHandler() override { return this; }
  CefRefPtr<CefDialogHandler> GetDialogHandler() override { return this; }
  CefRefPtr<CefDownloadHandler> GetDownloadHandler() override { return this; }

  const std::string& key() const { return key_; }
  void Rename(const std::string& key) { key_ = key; }
  CefRefPtr<CefBrowser> browser() const { return browser_; }
  void set_created_answer(int id) { created_answer_ = id; }

  // DevTools: `id` is Filo's request, answered with the method's result.
  void DevTools(int id, const std::string& method, CefRefPtr<CefDictionaryValue> params) {
    if (!browser_) {
      Answer(id, false, "The page isn't open yet.");
      return;
    }
    int message = browser_->GetHost()->ExecuteDevToolsMethod(0, method, params);
    if (message == 0) {
      Answer(id, false, "DevTools refused " + method);
      return;
    }
    waiting_[message] = id;
  }

  void Resize(int width, int height, float scale) {
    width_ = std::max(width, 1);
    height_ = std::max(height, 1);
    bool rescaled = scale != scale_;
    scale_ = scale;
    if (!browser_) return;
    if (rescaled) browser_->GetHost()->NotifyScreenInfoChanged();
    browser_->GetHost()->WasResized();
  }

  // CefRenderHandler
  void GetViewRect(CefRefPtr<CefBrowser>, CefRect& rect) override { rect = CefRect(0, 0, width_, height_); }

  bool GetScreenInfo(CefRefPtr<CefBrowser>, CefScreenInfo& info) override {
    CefRect rect(0, 0, width_, height_);
    info.device_scale_factor = scale_;
    info.depth = 24;
    info.depth_per_component = 8;
    info.rect = rect;
    info.available_rect = rect;
    return true;
  }

  void OnPopupShow(CefRefPtr<CefBrowser> browser, bool show) override {
    popup_shown_ = show;
    if (!show) {
      popup_rect_ = CefRect();
      browser->GetHost()->Invalidate(PET_VIEW);
    }
  }

  void OnPopupSize(CefRefPtr<CefBrowser>, const CefRect& rect) override { popup_rect_ = rect; }

  void OnPaint(CefRefPtr<CefBrowser>, PaintElementType type, const RectList&, const void* buffer, int width,
               int height) override {
    if (type == PET_VIEW) {
      if (!EnsureFrame(width, height)) return;
      memcpy(frame_, buffer, static_cast<size_t>(width) * height * 4);
    } else {
      // A <select>'s menu: drawn over the page where Chromium put it.
      if (!frame_ || !popup_shown_) return;
      int left = static_cast<int>(popup_rect_.x * scale_), top = static_cast<int>(popup_rect_.y * scale_);
      for (int row = 0; row < height; row++) {
        int y = top + row;
        if (y < 0 || y >= frame_height_) continue;
        int from = std::max(0, -left), to = std::min(width, frame_width_ - left);
        if (to <= from) continue;
        memcpy(frame_ + (static_cast<size_t>(y) * frame_width_ + left + from) * 4,
               static_cast<const uint8_t*>(buffer) + (static_cast<size_t>(row) * width + from) * 4,
               static_cast<size_t>(to - from) * 4);
      }
    }
    seq_++;
    Send("{\"ev\":\"frame\",\"tab\":" + Quote(key_) + ",\"shm\":" + Quote(shm_name_) +
         ",\"width\":" + std::to_string(frame_width_) + ",\"height\":" + std::to_string(frame_height_) +
         ",\"seq\":" + std::to_string(seq_) + "}");
  }

  // CefDisplayHandler
  void OnTitleChange(CefRefPtr<CefBrowser>, const CefString& title) override {
    title_ = title.ToString();
    SendState();
  }

  void OnAddressChange(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame, const CefString& url) override {
    if (frame->IsMain()) {
      url_ = url.ToString();
      SendState();
    }
  }

  bool OnCursorChange(CefRefPtr<CefBrowser>, CefCursorHandle, cef_cursor_type_t type,
                      const CefCursorInfo&) override {
    Send("{\"ev\":\"cursor\",\"tab\":" + Quote(key_) + ",\"type\":" + std::to_string(static_cast<int>(type)) + "}");
    return true;
  }

  // CefLoadHandler
  void OnLoadingStateChange(CefRefPtr<CefBrowser>, bool loading, bool back, bool forward) override {
    loading_ = loading;
    back_ = back;
    forward_ = forward;
    SendState();
  }

  // CefLifeSpanHandler
  bool OnBeforePopup(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, int, const CefString&, const CefString&,
                     WindowOpenDisposition, bool, const CefPopupFeatures&, CefWindowInfo& window_info,
                     CefRefPtr<CefClient>& client, CefBrowserSettings& settings, CefRefPtr<CefDictionaryValue>&,
                     bool*) override {
    // A pop-up (a sign-in's) or a link to a new window: a tab of its own, which Filo takes on with this key.
    std::string key = "p" + std::to_string(++g_popups);
    CefRefPtr<Tab> tab = new Tab(key, width_, height_, scale_, key_);
    g_tabs[key] = tab;
    window_info.SetAsWindowless(nullptr);
    settings.windowless_frame_rate = 30;
    settings.background_color = CefColorSetARGB(255, 255, 255, 255);
    client = tab;
    return false;
  }

  void OnAfterCreated(CefRefPtr<CefBrowser> browser) override {
    browser_ = browser;
    registration_ = browser->GetHost()->AddDevToolsMessageObserver(this);
    browser->GetHost()->WasHidden(false);
    // Focused as a tab in front is: it takes keys and shows a caret, and document.hasFocus() says so.
    browser->GetHost()->SetFocus(true);
    if (!opener_.empty()) {
      Send("{\"ev\":\"popup\",\"tab\":" + Quote(key_) + ",\"opener\":" + Quote(opener_) + ",\"url\":" +
           Quote(browser->GetMainFrame()->GetURL().ToString()) + "}");
    }
    Answer(created_answer_, true);
    created_answer_ = 0;
  }

  bool DoClose(CefRefPtr<CefBrowser>) override { return false; }

  void OnBeforeClose(CefRefPtr<CefBrowser>) override {
    registration_ = nullptr;
    for (auto& [message, id] : waiting_) Answer(id, false, "The tab closed.");
    waiting_.clear();
    browser_ = nullptr;
    DropFrame();
    if (!closed_by_filo_) Send("{\"ev\":\"closed\",\"tab\":" + Quote(key_) + "}");
    g_tabs.erase(key_);
    QuitWhenClosed();
  }

  void CloseByFilo() {
    closed_by_filo_ = true;
    if (browser_) browser_->GetHost()->CloseBrowser(true);
  }

  // CefRequestHandler
  void OnRenderProcessTerminated(CefRefPtr<CefBrowser>, TerminationStatus status, int,
                                 const CefString& error) override {
    Send("{\"ev\":\"crashed\",\"tab\":" + Quote(key_) + ",\"status\":" + std::to_string(static_cast<int>(status)) +
         ",\"error\":" + Quote(error.ToString()) + "}");
  }

  // CefJSDialogHandler: an alert, confirm or prompt waits for Filo's answer (the page's script waits meanwhile).
  bool OnJSDialog(CefRefPtr<CefBrowser>, const CefString&, JSDialogType type, const CefString& message,
                  const CefString& default_text, CefRefPtr<CefJSDialogCallback> callback, bool&) override {
    dialog_ = callback;
    const char* kind = type == JSDIALOGTYPE_ALERT ? "alert" : type == JSDIALOGTYPE_CONFIRM ? "confirm" : "prompt";
    Send("{\"ev\":\"dialog\",\"tab\":" + Quote(key_) + ",\"kind\":\"" + kind + "\",\"message\":" +
         Quote(message.ToString()) + ",\"default\":" + Quote(default_text.ToString()) + "}");
    return true;
  }

  // "Leave this page?": leave, as an agent's navigation means to.
  bool OnBeforeUnloadDialog(CefRefPtr<CefBrowser>, const CefString&, bool,
                            CefRefPtr<CefJSDialogCallback> callback) override {
    callback->Continue(true, CefString());
    return true;
  }

  void OnResetDialogState(CefRefPtr<CefBrowser>) override { dialog_ = nullptr; }

  void AnswerDialog(bool accept, const std::string& text) {
    if (!dialog_) return;
    CefRefPtr<CefJSDialogCallback> callback = dialog_;
    dialog_ = nullptr;
    callback->Continue(accept, text);
  }

  // CefDialogHandler: a file chooser waits for the files Filo gives.
  bool OnFileDialog(CefRefPtr<CefBrowser>, FileDialogMode mode, const CefString&, const CefString&,
                    const std::vector<CefString>&, const std::vector<CefString>&, const std::vector<CefString>&,
                    CefRefPtr<CefFileDialogCallback> callback) override {
    if (files_) files_->Cancel();
    files_ = callback;
    Send("{\"ev\":\"files\",\"tab\":" + Quote(key_) + ",\"multiple\":" +
         (mode == FILE_DIALOG_OPEN_MULTIPLE ? "true" : "false") + ",\"folder\":" +
         (mode == FILE_DIALOG_OPEN_FOLDER ? "true" : "false") + "}");
    return true;
  }

  void AnswerFiles(const std::vector<CefString>& paths) {
    if (!files_) return;
    CefRefPtr<CefFileDialogCallback> callback = files_;
    files_ = nullptr;
    if (paths.empty()) callback->Cancel();
    else callback->Continue(paths);
  }

  // CefDownloadHandler: Filo says where each download goes.
  bool OnBeforeDownload(CefRefPtr<CefBrowser>, CefRefPtr<CefDownloadItem> item, const CefString& name,
                        CefRefPtr<CefBeforeDownloadCallback> callback) override {
    downloads_[item->GetId()] = callback;
    Send("{\"ev\":\"download\",\"tab\":" + Quote(key_) + ",\"item\":" + std::to_string(item->GetId()) +
         ",\"url\":" + Quote(item->GetURL().ToString()) + ",\"name\":" + Quote(name.ToString()) + "}");
    return true;
  }

  void OnDownloadUpdated(CefRefPtr<CefBrowser>, CefRefPtr<CefDownloadItem> item,
                         CefRefPtr<CefDownloadItemCallback>) override {
    bool ended = item->IsComplete() || item->IsCanceled() || item->IsInterrupted();
    if (!ended || ended_.count(item->GetId())) return;
    ended_.insert(item->GetId());
    Send("{\"ev\":\"downloaded\",\"tab\":" + Quote(key_) + ",\"item\":" + std::to_string(item->GetId()) +
         ",\"url\":" + Quote(item->GetURL().ToString()) + ",\"ok\":" + (item->IsComplete() ? "true" : "false") +
         "}");
  }

  void AnswerDownload(uint32_t item, const std::string& path) {
    auto found = downloads_.find(item);
    if (found == downloads_.end()) return;
    CefRefPtr<CefBeforeDownloadCallback> callback = found->second;
    downloads_.erase(found);
    // An empty path would ask with a save panel: an unwanted download goes to a throwaway place instead.
    callback->Continue(path.empty() ? std::string("/dev/null") : path, false);
  }

  // CefDevToolsMessageObserver
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>, int message, bool success, const void* result,
                              size_t size) override {
    auto found = waiting_.find(message);
    if (found == waiting_.end()) return;
    int id = found->second;
    waiting_.erase(found);
    std::string json(static_cast<const char*>(result), size);
    if (success) {
      Send("{\"id\":" + std::to_string(id) + ",\"ok\":true,\"result\":" + (json.empty() ? "{}" : json) + "}");
    } else {
      Send("{\"id\":" + std::to_string(id) + ",\"ok\":false,\"error\":" + Quote(json) + "}");
    }
  }

 private:
  void SendState() {
    Send("{\"ev\":\"state\",\"tab\":" + Quote(key_) + ",\"title\":" + Quote(title_) + ",\"url\":" + Quote(url_) +
         ",\"loading\":" + (loading_ ? "true" : "false") + ",\"back\":" + (back_ ? "true" : "false") +
         ",\"forward\":" + (forward_ ? "true" : "false") + "}");
  }

  // The frame's shared memory, made again when its size changes (macOS can't resize a shared memory object).
  bool EnsureFrame(int width, int height) {
    if (frame_ && width == frame_width_ && height == frame_height_) return true;
    DropFrame();
    shm_name_ = "/filo-cr-" + std::to_string(getpid()) + "-" + std::to_string(++g_frames);
    int fd = shm_open(shm_name_.c_str(), O_CREAT | O_RDWR | O_EXCL, 0600);
    if (fd < 0) return false;
    size_t size = static_cast<size_t>(width) * height * 4;
    if (ftruncate(fd, size) != 0) {
      close(fd);
      shm_unlink(shm_name_.c_str());
      return false;
    }
    void* memory = mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (memory == MAP_FAILED) {
      shm_unlink(shm_name_.c_str());
      return false;
    }
    frame_ = static_cast<uint8_t*>(memory);
    frame_width_ = width;
    frame_height_ = height;
    return true;
  }

  void DropFrame() {
    if (!frame_) return;
    munmap(frame_, static_cast<size_t>(frame_width_) * frame_height_ * 4);
    shm_unlink(shm_name_.c_str());
    frame_ = nullptr;
  }

  static inline int g_frames = 0;

  std::string key_;
  int width_, height_;
  float scale_;
  std::string opener_;
  CefRefPtr<CefBrowser> browser_;
  CefRefPtr<CefRegistration> registration_;
  std::map<int, int> waiting_;
  int created_answer_ = 0;
  bool closed_by_filo_ = false;
  std::string title_, url_;
  bool loading_ = false, back_ = false, forward_ = false;
  uint8_t* frame_ = nullptr;
  int frame_width_ = 0, frame_height_ = 0;
  std::string shm_name_;
  uint64_t seq_ = 0;
  bool popup_shown_ = false;
  CefRect popup_rect_;
  CefRefPtr<CefJSDialogCallback> dialog_;
  CefRefPtr<CefFileDialogCallback> files_;
  std::map<uint32_t, CefRefPtr<CefBeforeDownloadCallback>> downloads_;
  std::set<uint32_t> ended_;

  IMPLEMENT_REFCOUNTING(Tab);
};

void QuitWhenClosed() {
  if (g_quitting && g_tabs.empty()) CefQuitMessageLoop();
}

void Quit() {
  if (g_quitting) return;
  g_quitting = true;
  CefCookieManager::GetGlobalManager(nullptr)->FlushStore(nullptr);
  if (g_tabs.empty()) {
    CefQuitMessageLoop();
    return;
  }
  for (auto& [key, tab] : std::map<std::string, CefRefPtr<Tab>>(g_tabs)) tab->CloseByFilo();
  // Whatever doesn't close in time.
  CefPostDelayedTask(TID_UI, base::BindOnce(&CefQuitMessageLoop), 3000);
}

std::string Text(CefRefPtr<CefDictionaryValue> request, const char* key) {
  return request->HasKey(key) ? request->GetString(key).ToString() : std::string();
}

double Number(CefRefPtr<CefDictionaryValue> request, const char* key, double otherwise) {
  if (!request->HasKey(key)) return otherwise;
  auto type = request->GetType(key);
  if (type == VTYPE_INT) return request->GetInt(key);
  if (type == VTYPE_DOUBLE) return request->GetDouble(key);
  return otherwise;
}

void Handle(const std::string& line) {
  CefRefPtr<CefValue> value = CefParseJSON(line, JSON_PARSER_RFC);
  if (!value || value->GetType() != VTYPE_DICTIONARY) return;
  CefRefPtr<CefDictionaryValue> request = value->GetDictionary();
  int id = static_cast<int>(Number(request, "id", 0));
  std::string op = Text(request, "op");
  std::string key = Text(request, "tab");
  if (op == "quit") {
    Quit();
    return;
  }
  if (op == "create") {
    if (g_tabs.count(key)) {
      Answer(id, false, "There's a tab " + key + " already.");
      return;
    }
    int width = static_cast<int>(Number(request, "width", 1280));
    int height = static_cast<int>(Number(request, "height", 800));
    float scale = static_cast<float>(Number(request, "scale", 2));
    CefRefPtr<Tab> tab = new Tab(key, width, height, scale, std::string());
    tab->set_created_answer(id);
    g_tabs[key] = tab;
    CefWindowInfo window_info;
    window_info.SetAsWindowless(nullptr);
    CefBrowserSettings settings;
    settings.windowless_frame_rate = 30;
    // A page with no background of its own is white, as in a browser (windowless pages are clear otherwise).
    settings.background_color = CefColorSetARGB(255, 255, 255, 255);
    std::string url = Text(request, "url");
    if (!CefBrowserHost::CreateBrowser(window_info, tab, url.empty() ? "about:blank" : url, settings, nullptr,
                                       nullptr)) {
      g_tabs.erase(key);
      Answer(id, false, "Chromium didn't make the page.");
    }
    return;
  }
  if (op == "rename") {
    // A pop-up, numbered as Filo numbers its tabs.
    auto found = g_tabs.find(key);
    std::string to = Text(request, "to");
    if (found != g_tabs.end() && !to.empty() && !g_tabs.count(to)) {
      CefRefPtr<Tab> tab = found->second;
      g_tabs.erase(found);
      tab->Rename(to);
      g_tabs[to] = tab;
    }
    Answer(id, true);
    return;
  }
  auto found = g_tabs.find(key);
  if (found == g_tabs.end() || !found->second->browser()) {
    Answer(id, false, "There's no tab " + key + " in Chromium.");
    return;
  }
  CefRefPtr<Tab> tab = found->second;
  CefRefPtr<CefBrowser> browser = tab->browser();
  if (op == "cdp") {
    CefRefPtr<CefDictionaryValue> params =
        request->HasKey("params") ? request->GetDictionary("params") : CefDictionaryValue::Create();
    tab->DevTools(id, Text(request, "method"), params);
  } else if (op == "load") {
    browser->GetMainFrame()->LoadURL(Text(request, "url"));
    Answer(id, true);
  } else if (op == "go") {
    std::string how = Text(request, "how");
    if (how == "back") browser->GoBack();
    else if (how == "forward") browser->GoForward();
    else if (how == "stop") browser->StopLoad();
    else browser->Reload();
    Answer(id, true);
  } else if (op == "resize") {
    tab->Resize(static_cast<int>(Number(request, "width", 1280)), static_cast<int>(Number(request, "height", 800)),
                static_cast<float>(Number(request, "scale", 2)));
    Answer(id, true);
  } else if (op == "dialog") {
    tab->AnswerDialog(request->HasKey("accept") && request->GetBool("accept"), Text(request, "text"));
    Answer(id, true);
  } else if (op == "files") {
    std::vector<CefString> paths;
    if (request->HasKey("paths")) {
      CefRefPtr<CefListValue> list = request->GetList("paths");
      for (size_t index = 0; index < list->GetSize(); index++) paths.push_back(list->GetString(index));
    }
    tab->AnswerFiles(paths);
    Answer(id, true);
  } else if (op == "download") {
    tab->AnswerDownload(static_cast<uint32_t>(Number(request, "item", 0)), Text(request, "path"));
    Answer(id, true);
  } else if (op == "focus") {
    browser->GetHost()->SetFocus(true);
    Answer(id, true);
  } else if (op == "close") {
    tab->CloseByFilo();
    Answer(id, true);
  } else {
    Answer(id, false, "Unknown op " + op);
  }
}

// Filo's requests, read on a thread of their own and handled on CEF's UI thread; the app quits when Filo goes.
void Read() {
  std::string line;
  while (std::getline(std::cin, line)) {
    if (line.empty()) continue;
    CefPostTask(TID_UI, base::BindOnce(&Handle, line));
  }
  CefPostTask(TID_UI, base::BindOnce(&Quit));
}

class App : public CefApp, public CefBrowserProcessHandler {
 public:
  CefRefPtr<CefBrowserProcessHandler> GetBrowserProcessHandler() override { return this; }

  void OnBeforeCommandLineProcessing(const CefString& process_type, CefRefPtr<CefCommandLine> line) override {
    if (!process_type.empty()) return;
    // The login keychain isn't asked for the cookie encryption key (macOS would ask the user): cookies are kept in
    // the profile's folder as they are, like a test profile's.
    line->AppendSwitch("use-mock-keychain");
  }

  void OnContextInitialized() override {
    Send("{\"ev\":\"ready\",\"version\":" + Quote(CEF_VERSION) + "}");
    std::thread(Read).detach();
  }

  IMPLEMENT_REFCOUNTING(App);
};

}  // namespace

// CEF asks the app's NSApplication to say when it's handling an event (CefAppProtocol).
@interface FiloChromiumApplication : NSApplication <CefAppProtocol> {
  BOOL handlingSendEvent_;
}
@end

@implementation FiloChromiumApplication
- (BOOL)isHandlingSendEvent {
  return handlingSendEvent_;
}
- (void)setHandlingSendEvent:(BOOL)handlingSendEvent {
  handlingSendEvent_ = handlingSendEvent;
}
- (void)sendEvent:(NSEvent*)event {
  CefScopedSendingEvent sendingEventScoper;
  [super sendEvent:event];
}
@end

int main(int argc, char* argv[]) {
#if defined(FILO_HELPER)
  CefScopedLibraryLoader library;
  if (!library.LoadInHelper()) return 1;
  CefMainArgs main_args(argc, argv);
  return CefExecuteProcess(main_args, nullptr, nullptr);
#else
  CefScopedLibraryLoader library;
  if (!library.LoadInMain()) return 1;
  CefMainArgs main_args(argc, argv);

  g_out = dup(STDOUT_FILENO);
  dup2(STDERR_FILENO, STDOUT_FILENO);

  std::string profile;
  for (int index = 1; index < argc; index++) {
    std::string arg = argv[index];
    if (arg.rfind("--filo-profile=", 0) == 0) profile = arg.substr(15);
  }

  @autoreleasepool {
    [FiloChromiumApplication sharedApplication];
    // No Dock icon, no menu bar: Filo is the app the user sees.
    [NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];

    CefSettings settings;
    settings.no_sandbox = true;
    settings.windowless_rendering_enabled = true;
    settings.persist_session_cookies = false;
    settings.log_severity = LOGSEVERITY_WARNING;
    if (!profile.empty()) {
      CefString(&settings.root_cache_path) = profile;
      CefString(&settings.cache_path) = profile + "/Default";
    }
    CefRefPtr<App> app(new App);
    if (!CefInitialize(main_args, settings, app, nullptr)) return CefGetExitCode();
    CefRunMessageLoop();
    CefShutdown();
  }
  return 0;
#endif
}
