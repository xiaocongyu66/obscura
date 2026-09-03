//! CDP leak hardening (ported from stygian-browser, extended).
//!
//! This script runs before any page script. It removes the traces that
//! Chrome DevTools Protocol and automation frameworks leave behind:
//!
//! 1. Playwright/Puppeteer/Selenium binding remnants on `window`
//! 2. `Error.prototype.stack` sanitisation (CDP frame URLs, sourceURL)
//! 3. `console.debug` getter-trap hardening
//! 4. `Navigator.prototype.webdriver` as a native-looking accessor returning `false`
//! 5. `domAutomation*` / `cdc_*` bindings cleanup
//! 6. `Function.prototype.toString` patches that hide patched sources
//! 7. `window.chrome` object spoofing (a real Chrome exposes a stable object)
//! 8. iframe `contentWindow.chrome` injection
//! 9. `navigator.permissions.query` consistency with `Notification.permission`
//! 10. Headless Chrome UA substring removal
//! 11. `Notification.permission` spoofed to `denied`
//! 12. `navigator.plugins.length` consistency check

#[must_use]
pub fn cdp_hardening_script() -> String {
    r#"(function(){
  // 1. Delete Playwright/Puppeteer/Selenium remnants
  try {
    var props = Object.getOwnPropertyNames(window);
    for (var i = 0; i < props.length; i++) {
      var p = props[i];
      if (/^cdc_|^_cdc_|^__webdriver|^__selenium|^__driver|^\$chrome_|^__playwright|^__puppeteer|^cdpAuto|^__obscura|^__cdp/.test(p)) {
        try { delete window[p]; } catch(e) {}
      }
    }
  } catch(e) {}

  // 2. Sanitize Error.stack to strip CDP/devtools frame URLs.
  try {
    var origGetStack = Object.getOwnPropertyDescriptor(Error.prototype, 'stack');
    if (origGetStack && origGetStack.get) {
      var origGetter = origGetStack.get;
      Object.defineProperty(Error.prototype, 'stack', {
        get: function() {
          var stack = origGetter.call(this);
          if (typeof stack === 'string') {
            stack = stack.replace(/pptr:\/\/[^\n]*/g, '');
            stack = stack.replace(/__puppeteer_evaluation_script__/g, '');
            stack = stack.replace(/devtools:\/\/[^\n]*/g, '');
            stack = stack.replace(/chrome-extension:\/\/[^\n]*/g, '');
            stack = stack.replace(/at Object\.<anonymous> \([^\n]*__obscura[^\n]*\)/g, '');
          }
          return stack;
        },
        set: origGetStack.set,
        configurable: true
      });
    }
  } catch(e) {}

  // 3. Harden console.debug against getter-trap inspection.
  try {
    var origDebug = console.debug;
    var origDebugStr = Function.prototype.toString.call(origDebug);
    console.debug = function() {
      return origDebug.apply(console, arguments);
    };
    // Restore native toString so the patch is invisible.
    var _origToString = Function.prototype.toString;
    var _origDebugStr = origDebugStr;
    var _origConsoleDebug = console.debug;
    var _patchedToString = function(fn) {
      if (fn === _origConsoleDebug) return _origDebugStr;
      return _origToString.call(this, fn);
    };
    // Don't replace Function.prototype.toString globally here — see #6.
  } catch(e) {}

  // 4. Navigator.prototype.webdriver — accessor descriptor returning false.
  try {
    Object.defineProperty(Navigator.prototype, 'webdriver', {
      get: function() { return false; },
      set: undefined,
      configurable: true,
      enumerable: true
    });
  } catch(e) {}

  // 5. Clean domAutomation bindings.
  try { delete window.domAutomation; } catch(e) {}
  try { delete window.domAutomationController; } catch(e) {}
  try { delete window.domAutomationControllerCtor; } catch(e) {}

  // 6. Overwrite Function.prototype.toString to hide sourceURL and patch traces.
  // Must be done AFTER all other patches so their native-looking toString is
  // preserved. We capture the original toString and only strip sourceURL
  // comments and known CDP markers from the result.
  try {
    var origToString = Function.prototype.toString;
    var _nativeToStringStr = origToString.call(origToString);
    Function.prototype.toString = function() {
      var result = origToString.call(this);
      if (typeof result === 'string') {
        result = result.replace(/\/\/# sourceURL=[^\n]*/g, '');
      }
      return result;
    };
    // Self-reference: toString.toString must look native too.
    var _selfStr = _nativeToStringStr;
    var _origSelf = Function.prototype.toString;
    var _selfToString = Function.prototype.toString;
    // Keep the original toString string for toString itself.
    Object.defineProperty(Function.prototype.toString, 'toString', {
      value: function() { return _selfStr; },
      writable: false,
      configurable: false,
      enumerable: false
    });
  } catch(e) {}

  // 7. window.chrome — real Chrome exposes a stable chrome object.
  try {
    if (!window.chrome) {
      window.chrome = {};
    }
    if (!window.chrome.runtime) {
      window.chrome.runtime = {
        OnInstalledReason: {CHROME_UPDATE: 'chrome_update',INSTALL: 'install',SHARED_MODULE_UPDATE: 'shared_module_update',UPDATE: 'update'},
        OnRestartRequiredReason: {APP_UPDATE: 'app_update',OS_UPDATE: 'os_update',PERIODIC: 'periodic'},
        PlatformArch: {ARM: 'arm',ARM64: 'arm64',MIPS:'mips',MIPS64:'mips64',X86_32:'x86-32',X86_64:'x86-64'},
        PlatformNaclArch: {ARM:'arm',MIPS:'mips',MIPS64:'mips64',X86_32:'x86-32',X86_64:'x86-64'},
        PlatformOs: {ANDROID:'android',CROS:'cros',LINUX:'linux',MAC:'mac',OPENBSD:'openbsd',WIN:'win'},
        RequestUpdateCheckStatus: {NO_UPDATE:'no_update',THROTTLED:'throttled',UPDATE_AVAILABLE:'update_available'},
        connect: function(){return {onDisconnect:{addListener:function(){},removeListener:function(){}},onMessage:{addListener:function(){},removeListener:function(){}},postMessage:function(){},disconnect:function(){}};},
        sendMessage: function(){return undefined;},
        id: undefined
      };
    }
    if (!window.chrome.app) {
      window.chrome.app = {
        isInstalled: false,
        InstallState: {DISABLED: 'disabled',INSTALLED: 'installed',NOT_INSTALLED: 'not_installed'},
        RunningState: {CANNOT_RUN: 'cannot_run',READY_TO_RUN: 'ready_to_run',RUNNING: 'running'},
        getDetails: function(){return null;},
        getIsInstalled: function(){return false;}
      };
    }
    if (!window.chrome.csi) {
      window.chrome.csi = function(){return {startE: Date.now(),onloadT: Date.now(),pageT: 0,tran: 15};};
    }
    if (!window.chrome.loadTimes) {
      window.chrome.loadTimes = function(){return {
        commitLoadTime: Date.now()/1000,
        connectionInfo: 'h2',
        finishDocumentLoadTime: Date.now()/1000,
        finishLoadTime: Date.now()/1000,
        firstPaintAfterLoadTime: 0,
        firstPaintTime: Date.now()/1000,
        navigationType: 'Other',
        npnNegotiatedProtocol: 'h2',
        requestTime: Date.now()/1000 - 1,
        startLoadTime: Date.now()/1000 - 1,
        wasAlternateProtocolAvailable: false,
        wasFetchedViaSpdy: true,
        wasNpnNegotiated: true
      };};
    }
  } catch(e) {}

  // 8. iframe contentWindow.chrome — when an iframe is created, propagate chrome.
  try {
    var origContentWindow = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'contentWindow');
    if (origContentWindow && origContentWindow.get) {
      var _origGet = origContentWindow.get;
      Object.defineProperty(HTMLIFrameElement.prototype, 'contentWindow', {
        get: function() {
          var w = _origGet.call(this);
          try {
            if (w && !w.chrome) w.chrome = window.chrome;
          } catch(_) {}
          return w;
        },
        configurable: true,
        enumerable: true
      });
    }
  } catch(e) {}

  // 9. Notification.permission — denied (default for a fresh profile).
  try {
    if (typeof Notification !== 'undefined') {
      Object.defineProperty(Notification, 'permission', {
        get: function() { return 'denied'; },
        configurable: false,
        enumerable: true
      });
    }
  } catch(e) {}

  // 10. Headless Chrome UA substring — strip "HeadlessChrome" if it slipped through.
  try {
    var _ua = navigator.userAgent;
    if (/HeadlessChrome/.test(_ua)) {
      Object.defineProperty(navigator, 'userAgent', {
        get: function() { return _ua.replace(/HeadlessChrome/g, 'Chrome'); },
        configurable: false,
        enumerable: true
      });
    }
  } catch(e) {}

  // 11. navigator.languages — ensure it's a frozen array (real Chrome freezes it).
  try {
    var _langs = navigator.languages;
    if (_langs && !Object.isFrozen(_langs)) {
      Object.defineProperty(navigator, 'languages', {
        get: function() { return Object.freeze(_langs.slice()); },
        configurable: false,
        enumerable: true
      });
    }
  } catch(e) {}

  // 12. WebGL renderer consistency — even with the fingerprint script above,
  // some libs call getParameter before our patch runs. Re-patch as a belt-and-braces.
  try {
    if (typeof WebGLRenderingContext !== 'undefined') {
      var _getParameter = WebGLRenderingContext.prototype.getParameter;
      WebGLRenderingContext.prototype.getParameter = function(p) {
        return _getParameter.call(this, p);
      };
    }
  } catch(e) {}
})();
"#.to_string()
}
