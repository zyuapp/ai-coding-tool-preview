//! The Objective-C objects WebKit reports a page through: what it may load, how loading went, and
//! the properties of the view that change as it does.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, MainThreadOnly};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSDictionary, NSError, NSHTTPURLResponse, NSKeyValueChangeKey, NSKeyValueObservingOptions,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSString,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationResponse, WKNavigationResponsePolicy, WKUIDelegate,
    WKWebView, WKWebViewConfiguration, WKWindowFeatures,
};

/// What WebKit said about a page.
pub enum Signal<'a> {
    /// A navigation about to happen. The answer is whether it may.
    Decide { url: String, main_frame: bool, method: String },
    Started,
    Response { url: String, status: Option<u16>, main_frame: bool },
    Committed,
    Finished,
    Failed { error: &'a NSError, provisional: bool },
    Terminated,
    /// The page asked to close its own window.
    Closed,
}

pub type SignalHandler = Box<dyn Fn(&WKWebView, Signal<'_>) -> bool>;
pub type PropertyHandler = Box<dyn Fn(&WKWebView, &str)>;

pub struct DelegateIvars {
    handler: SignalHandler,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateIvars]
    pub struct PageDelegate;

    unsafe impl NSObjectProtocol for PageDelegate {}

    unsafe impl WKNavigationDelegate for PageDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_action(&self, webview: &WKWebView, action: &WKNavigationAction, handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>) {
            let request = unsafe { action.request() };
            let url = request.URL().and_then(|url| url.absoluteString()).map(|url| url.to_string()).unwrap_or_default();
            let method = request.HTTPMethod().map(|method| method.to_string()).unwrap_or_else(|| "GET".into());
            // A navigation with no target frame opens a window, which the UI delegate answers.
            let main_frame = unsafe { action.targetFrame() }.is_none_or(|frame| unsafe { frame.isMainFrame() });
            let allowed = (self.ivars().handler)(webview, Signal::Decide { url, main_frame, method });
            handler.call((if allowed { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(&self, webview: &WKWebView, response: &WKNavigationResponse, handler: &block2::DynBlock<dyn Fn(WKNavigationResponsePolicy)>) {
            let main_frame = unsafe { response.isForMainFrame() };
            let answer = unsafe { response.response() };
            let url = answer.URL().and_then(|url| url.absoluteString()).map(|url| url.to_string()).unwrap_or_default();
            let status = answer.downcast_ref::<NSHTTPURLResponse>().map(|http| http.statusCode() as u16);
            (self.ivars().handler)(webview, Signal::Response { url, status, main_frame });
            // The panel shows pages; a file it cannot show is not downloaded behind the user's back.
            let showable = unsafe { response.canShowMIMEType() };
            handler.call((if showable { WKNavigationResponsePolicy::Allow } else { WKNavigationResponsePolicy::Cancel },));
        }

        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn did_start(&self, webview: &WKWebView, _navigation: Option<&WKNavigation>) {
            (self.ivars().handler)(webview, Signal::Started);
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, webview: &WKWebView, _navigation: Option<&WKNavigation>) {
            (self.ivars().handler)(webview, Signal::Committed);
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, webview: &WKWebView, _navigation: Option<&WKNavigation>) {
            (self.ivars().handler)(webview, Signal::Finished);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(&self, webview: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            (self.ivars().handler)(webview, Signal::Failed { error, provisional: true });
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, webview: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            (self.ivars().handler)(webview, Signal::Failed { error, provisional: false });
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, webview: &WKWebView) {
            (self.ivars().handler)(webview, Signal::Terminated);
        }
    }

    unsafe impl WKUIDelegate for PageDelegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            _webview: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            _action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            None
        }

        #[unsafe(method(webViewDidClose:))]
        fn did_close(&self, webview: &WKWebView) {
            (self.ivars().handler)(webview, Signal::Closed);
        }
    }
);

impl PageDelegate {
    pub fn new(mtm: MainThreadMarker, handler: SignalHandler) -> Retained<Self> {
        let delegate = mtm.alloc::<PageDelegate>().set_ivars(DelegateIvars { handler });
        unsafe { msg_send![super(delegate), init] }
    }

    pub fn attach(&self, webview: &WKWebView, ui: bool) {
        unsafe {
            webview.setNavigationDelegate(Some(ProtocolObject::from_ref(self)));
            if ui {
                webview.setUIDelegate(Some(ProtocolObject::from_ref(self)));
            }
        }
    }
}

/// The view's properties a panel shows: whether it is loading, its address and title, and where it can go.
pub const OBSERVED_KEYS: [&str; 5] = ["loading", "title", "URL", "canGoBack", "canGoForward"];

pub struct ObserverIvars {
    object: Retained<WKWebView>,
    handler: PropertyHandler,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = ObserverIvars]
    pub struct PropertyObserver;

    impl PropertyObserver {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(&self, key_path: Option<&NSString>, _object: Option<&AnyObject>, _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>, _context: *mut c_void) {
            if let Some(key) = key_path {
                (self.ivars().handler)(&self.ivars().object, &key.to_string());
            }
        }
    }

    unsafe impl NSObjectProtocol for PropertyObserver {}
);

impl PropertyObserver {
    pub fn new(object: Retained<WKWebView>, handler: PropertyHandler) -> Retained<Self> {
        let observer = Self::alloc().set_ivars(ObserverIvars { object, handler });
        let observer: Retained<Self> = unsafe { msg_send![super(observer), init] };
        for key in OBSERVED_KEYS {
            unsafe {
                observer.ivars().object.addObserver_forKeyPath_options_context(&observer, &NSString::from_str(key), NSKeyValueObservingOptions::New, std::ptr::null_mut());
            }
        }
        observer
    }

    /// Stops observing. Called before the view goes, since KVO keeps no strong reference to the observer.
    pub fn detach(&self) {
        for key in OBSERVED_KEYS {
            unsafe { self.ivars().object.removeObserver_forKeyPath(self, &NSString::from_str(key)) };
        }
    }
}

/// The message a failed load reads as, or None for a load that was only replaced or refused.
pub fn failure_message(error: &NSError) -> Option<String> {
    let code = error.code();
    let domain = error.domain().to_string();
    // A navigation cut short by another, or refused by the panel itself, is not a failure to show.
    if domain == "NSURLErrorDomain" && code == -999 {
        return None;
    }
    if domain == "WebKitErrorDomain" && (code == 102 || code == 204) {
        return None;
    }
    let description = error.localizedDescription().to_string();
    let failing = error
        .userInfo()
        .objectForKey(ns_string!("NSErrorFailingURLStringKey"))
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|value| value.to_string())
        .unwrap_or_default();
    Some(if failing.is_empty() { description } else { format!("{description} ({failing})") })
}
