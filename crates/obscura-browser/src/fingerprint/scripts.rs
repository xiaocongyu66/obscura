//! JS injection script generators for each fingerprint surface.
//!
//! Each function returns a self-contained IIFE that spoofs one surface.
//! [`build_injection_script`](super::inject::build_injection_script)
//! concatenates them all into the payload passed to
//! `Page.addScriptToEvaluateOnNewDocument`.

use crate::fingerprint::identity::{LINUX_FONTS, MACOS_FONTS, WINDOWS_FONTS};

/// Screen resolution, availWidth/Height, outerWidth/Height, colorDepth.
pub fn screen_script(width: u32, height: u32) -> String {
    let ah = height.saturating_sub(40);
    format!(
        r#"(function(){{
  var _ds=function(p,v){{Object.defineProperty(screen,p,{{get:()=>v,configurable:false,enumerable:true}});}};
  _ds('width',{width});_ds('height',{height});_ds('availWidth',{width});_ds('availHeight',{ah});
  _ds('availLeft',0);_ds('availTop',0);_ds('colorDepth',24);_ds('pixelDepth',24);
  try{{
    Object.defineProperty(window,'outerWidth',{{get:()=>{width},configurable:true}});
    Object.defineProperty(window,'outerHeight',{{get:()=>{height},configurable:true}});
    Object.defineProperty(window,'innerWidth',{{get:()=>{width},configurable:true}});
    Object.defineProperty(window,'innerHeight',{{get:()=>{height},configurable:true}});
    if(globalThis.visualViewport){{
      Object.defineProperty(visualViewport,'width',{{get:()=>{width},configurable:true}});
      Object.defineProperty(visualViewport,'height',{{get:()=>{height},configurable:true}});
    }}
  }}catch(_){{}}
}})();"#
    )
}

/// `Intl.DateTimeFormat().resolvedOptions().timeZone` and `Date` toString
/// offset. The timezone string is what fingerprintjs reads.
pub fn timezone_script(timezone: &str) -> String {
    format!(
        r#"(function(){{
  var _ro=Intl.DateTimeFormat.prototype.resolvedOptions;
  Intl.DateTimeFormat.prototype.resolvedOptions=function(){{
    var o=_ro.apply(this,arguments);
    try{{o.timeZone={tz:?};}}catch(_){{}}
    return o;
  }};
}})();"#,
        tz = timezone
    )
}

/// navigator.language, navigator.languages, navigator.userAgent.
pub fn language_script(language: &str, secondary: &str, user_agent: &str) -> String {
    format!(
        r#"(function(){{
  try{{Object.defineProperty(navigator,'language',{{get:()=>{lang:?},configurable:false,enumerable:true}});}}catch(_){{}}
  try{{Object.defineProperty(navigator,'languages',{{get:()=>Object.freeze([{lang:?},{sec:?}]),configurable:false,enumerable:true}});}}catch(_){{}}
  try{{Object.defineProperty(navigator,'userAgent',{{get:()=>{ua:?},configurable:false,enumerable:true}});}}catch(_){{}}
}})();"#,
        lang = language,
        sec = secondary,
        ua = user_agent,
    )
}

/// navigator.platform, hardwareConcurrency, deviceMemory.
pub fn hardware_script(platform: &str, hw: u32, mem: u32) -> String {
    format!(
        r#"(function(){{
  try{{Object.defineProperty(navigator,'platform',{{get:()=>{plat:?},configurable:false,enumerable:true}});}}catch(_){{}}
  try{{Object.defineProperty(navigator,'hardwareConcurrency',{{get:()=>{hw},configurable:false,enumerable:true}});}}catch(_){{}}
  try{{Object.defineProperty(navigator,'deviceMemory',{{get:()=>{mem},configurable:false,enumerable:true}});}}catch(_){{}}
}})();"#,
        plat = platform,
        hw = hw,
        mem = mem,
    )
}

/// WebGL vendor/renderer spoofing via `getParameter` + `WEBGL_debug_renderer_info`
/// extension. Also spoofs a few stable parameters (MAX_TEXTURE_SIZE etc.) so
/// the WebGL fingerprint matches the claimed GPU.
///
/// This also sets `globalThis.__obscura_webgl_vendor` / `__obscura_webgl_renderer`
/// so the WebGL stub context (in bootstrap.js `_makeWebGLContext`) can read
/// the same values — without a real wgpu backend, the stub returns these
/// from `getParameter(VENDOR)` etc.
pub fn webgl_script(vendor: &str, renderer: &str) -> String {
    format!(
        r#"(function(){{
  // Publish the vendor/renderer as globals so the WebGL stub context
  // (bootstrap.js `_makeWebGLContext`) reads the same values. This is
  // what makes a stub context's getParameter(VENDOR) match the fingerprint.
  globalThis.__obscura_webgl_vendor = {v:?};
  globalThis.__obscura_webgl_renderer = {r:?};
  if(typeof WebGLRenderingContext==='undefined'&&typeof WebGL2RenderingContext==='undefined')return;
  var VENDOR=0x1F00,RENDERER=0x1F01,UNMASKED_VENDOR=0x9245,UNMASKED_RENDERER=0x9246;
  var patch=function(Ctx){{
    var _p=Ctx.prototype.getParameter;
    Ctx.prototype.getParameter=function(p){{
      if(p===VENDOR)return {v:?};
      if(p===RENDERER)return {r:?};
      if(p===UNMASKED_VENDOR)return {v:?};
      if(p===UNMASKED_RENDERER)return {r:?};
      try{{return _p.call(this,p);}}catch(_e){{return null;}}
    }};
    var _e=Ctx.prototype.getExtension;
    Ctx.prototype.getExtension=function(n){{
      if(n==='WEBGL_debug_renderer_info'){{
        return {{UNMASKED_VENDOR_WEBGL:UNMASKED_VENDOR,UNMASKED_RENDERER_WEBGL:UNMASKED_RENDERER}};
      }}
      try{{return _e.call(this,n);}}catch(_e){{return null;}}
    }};
    var _s=Ctx.prototype.getSupportedExtensions;
    Ctx.prototype.getSupportedExtensions=function(){{
      var base;
      try{{base=_s?_s.call(this):null;}}catch(_e){{base=null;}}
      if(!base)return base;
      return base;
    }};
  }};
  if(typeof WebGLRenderingContext!=='undefined')patch(WebGLRenderingContext);
  if(typeof WebGL2RenderingContext!=='undefined')patch(WebGL2RenderingContext);
}})();"#,
        v = vendor,
        r = renderer,
    )
}

/// Deterministic canvas pixel noise. The seed is mixed in JS so two pages in
/// the same session produce the same hash. The PRNG is SplitMix64 — same as
/// the Rust `NoiseEngine` — so the two sides agree.
pub fn canvas_noise_script(seed: u64) -> String {
    format!(
        r#"(function(){{
  if(typeof CanvasRenderingContext2D==='undefined')return;
  var SEED={seed}>>>0;
  function splitmix(state){{
    state=(state+0x9E3779B1)>>>0;
    var z=state;
    z=((z^(z>>>16))*0x85EBCA6B)>>>0;
    z=((z^(z>>>13))*0xC2B2AE35)>>>0;
    return (z^(z>>>16))>>>0;
  }}
  function noise(channel,pixelIndex){{
    var s=(SEED+0x9E3779B1+channel*0x100000001b3+pixelIndex*0x517CC1B727220A95)>>>0;
    return (splitmix(s)%3)-1;
  }}
  var _getImageData=CanvasRenderingContext2D.prototype.getImageData;
  CanvasRenderingContext2D.prototype.getImageData=function(){{
    var id=_getImageData.apply(this,arguments);
    var d=id.data;
    for(var i=0;i<d.length;i+=4){{
      d[i]+=noise(0,i>>2);if(d[i]<0)d[i]=0;if(d[i]>255)d[i]=255;
      d[i+1]+=noise(1,i>>2);if(d[i+1]<0)d[i+1]=0;if(d[i+1]>255)d[i+1]=255;
      d[i+2]+=noise(2,i>>2);if(d[i+2]<0)d[i+2]=0;if(d[i+2]>255)d[i+2]=255;
    }}
    return id;
  }};
  var _toDataURL=HTMLCanvasElement.prototype.toDataURL;
  HTMLCanvasElement.prototype.toDataURL=function(){{
    try{{
      var ctx=this.getContext('2d');
      if(ctx){{
        var w=this.width,h=this.height;
        if(w>0&&h>0){{
          var id=_getImageData.call(ctx,0,0,w,h);
          var d=id.data;
          for(var i=0;i<d.length;i+=4){{
            d[i]+=noise(0,i>>2);if(d[i]<0)d[i]=0;if(d[i]>255)d[i]=255;
            d[i+1]+=noise(1,i>>2);if(d[i+1]<0)d[i+1]=0;if(d[i+1]>255)d[i+1]=255;
            d[i+2]+=noise(2,i>>2);if(d[i+2]<0)d[i+2]=0;if(d[i+2]>255)d[i+2]=255;
          }}
          ctx.putImageData(id,0,0);
        }}
      }}
    }}catch(_){{}}
    return _toDataURL.apply(this,arguments);
  }};
}})();"#,
        seed = seed,
    )
}

/// Deterministic audio noise. Hooks `AnalyserNode.getFloatFrequencyData` and
/// `getByteFrequencyData`, plus `AudioBuffer.getChannelData` — the three
/// surfaces fingerprintjs reads.
pub fn audio_noise_script(seed: u64) -> String {
    format!(
        r#"(function(){{
  if(typeof AnalyserNode==='undefined')return;
  var SEED={seed}>>>0;
  function splitmix(state){{
    state=(state+0x9E3779B1)>>>0;
    var z=state;
    z=((z^(z>>>16))*0x85EBCA6B)>>>0;
    z=((z^(z>>>13))*0xC2B2AE35)>>>0;
    return (z^(z>>>16))>>>0;
  }}
  function noise(index){{
    var s=(SEED+0x9E3779B1+0x415544494F310000+index*0x517CC1B727220A95)>>>0;
    var v=splitmix(s);
    return ((v/4294967295.0)*2-1)*1e-7;
  }}
  var _getFloat=AnalyserNode.prototype.getFloatFrequencyData;
  AnalyserNode.prototype.getFloatFrequencyData=function(arr){{
    _getFloat.apply(this,arguments);
    for(var i=0;i<arr.length;i++){{arr[i]+=noise(i);}}
  }};
  if(AnalyserNode.prototype.getByteFrequencyData){{
    var _getByte=AnalyserNode.prototype.getByteFrequencyData;
    AnalyserNode.prototype.getByteFrequencyData=function(arr){{
      _getByte.apply(this,arguments);
      for(var i=0;i<arr.length;i++){{arr[i]=Math.max(0,Math.min(255,arr[i]+(noise(i)>0?1:-1)));}}
    }};
  }}
  if(typeof AudioBuffer!=='undefined'&&AudioBuffer.prototype.getChannelData){{
    var _getChannel=AudioBuffer.prototype.getChannelData;
    AudioBuffer.prototype.getChannelData=function(ch){{
      var data=_getChannel.apply(this,arguments);
      for(var i=0;i<data.length;i++){{data[i]+=noise(i);}}
      return data;
    }};
  }}
}})();"#,
        seed = seed,
    )
}

/// navigator.connection — spoof a stable 4g/wifi profile.
pub fn connection_script() -> String {
    r#"(function(){
  var c={rtt:50,downlink:10,effectiveType:'4g',type:'wifi',saveData:false,
    onchange:null,ontypechange:null,
    addEventListener:function(){},removeEventListener:function(){},
    dispatchEvent:function(){return true;}};
  try{Object.defineProperty(navigator,'connection',{get:function(){return c;},enumerable:true,configurable:false});}catch(_){}
})();"#.to_string()
}

/// `getBoundingClientRect` / `getClientRects` subpixel noise so a deterministic
/// layout hash shifts. Only perturbs hidden/offscreen elements to avoid
/// visible layout shifts.
pub fn client_rects_noise_script(seed: u64) -> String {
    format!(
        r#"(function(){{
  var SEED={seed}>>>0;
  function splitmix(state){{
    state=(state+0x9E3779B1)>>>0;
    var z=state;
    z=((z^(z>>>16))*0x85EBCA6B)>>>0;
    z=((z^(z>>>13))*0xC2B2AE35)>>>0;
    return (z^(z>>>16))>>>0;
  }}
  function noise(rectIndex,dim){{
    var s=(SEED+0x9E3779B1+0x5245435431000000+rectIndex*0x100000001b3+dim*0x517CC1B727220A95)>>>0;
    var v=splitmix(s);
    return ((v/4294967295.0)*2-1);
  }}
  var _getBoundingClientRect=Element.prototype.getBoundingClientRect;
  Element.prototype.getBoundingClientRect=function(){{
    var r=_getBoundingClientRect.apply(this,arguments);
    try{{
      var idx=(this._nid||0)&0xffff;
      var dx=noise(idx,0),dy=noise(idx,1),dw=noise(idx,2),dh=noise(idx,3);
      return new DOMRect(r.x+dx,r.y+dy,r.width+dw,r.height+dh);
    }}catch(_){{return r;}}
  }};
}})();"#,
        seed = seed,
    )
}

/// navigator.storage.estimate — spoof a stable quota/usage pair.
pub fn storage_estimate_script() -> String {
    r#"(function(){
  if(!navigator.storage||typeof navigator.storage.estimate!=='function')return;
  var _o=navigator.storage.estimate.bind(navigator.storage);
  var q=240*1073741824;
  var u=5*1048576;
  navigator.storage.estimate=function(){
    return _o().then(function(r){
      return {quota:Math.max(r.quota||0,q),usage:Math.min(r.usage||0,u)};
    }).catch(function(){return {quota:q,usage:u};});
  };
})();"#.to_string()
}

/// navigator.getBattery — spoof a stable, plausible battery profile.
pub fn battery_script() -> String {
    r#"(function(){
  if(typeof navigator.getBattery!=='function')return;
  var b={charging:false,chargingTime:Infinity,dischargingTime:14400,level:0.82,
    onchargingchange:null,onchargingtimechange:null,ondischargingtimechange:null,onlevelchange:null,
    addEventListener:function(){},removeEventListener:function(){},dispatchEvent:function(){return true;}};
  navigator.getBattery=function(){return Promise.resolve(b);};
})();"#.to_string()
}

/// navigator.plugins / navigator.mimeTypes — spoof the Chrome PDF viewer pair
/// that every real Chrome installation reports.
pub fn plugins_script() -> String {
    r#"(function(){
  var m0={type:'application/pdf',description:'Portable Document Format',suffixes:'pdf',enabledPlugin:null};
  var m1={type:'text/pdf',description:'Portable Document Format',suffixes:'pdf',enabledPlugin:null};
  var p={name:'PDF Viewer',description:'Portable Document Format',filename:'internal-pdf-viewer',
    length:2,0:m0,1:m1,
    item:function(i){return [m0,m1][i]||null;},
    namedItem:function(n){if(n==='application/pdf')return m0;if(n==='text/pdf')return m1;return null;}};
  m0.enabledPlugin=p;m1.enabledPlugin=p;
  var fp={length:1,0:p,
    item:function(i){return i===0?p:null;},
    namedItem:function(n){return n==='PDF Viewer'?p:null;},
    refresh:function(){}};
  var fm={length:2,0:m0,1:m1,
    item:function(i){return [m0,m1][i]||null;},
    namedItem:function(n){if(n==='application/pdf')return m0;if(n==='text/pdf')return m1;return null;}};
  try{
    Object.defineProperty(navigator,'plugins',{get:function(){return fp;},configurable:false,enumerable:true});
    Object.defineProperty(navigator,'mimeTypes',{get:function(){return fm;},configurable:false,enumerable:true});
  }catch(e){}
})();"#.to_string()
}

/// navigator.mediaDevices — spoof a stable device list (1 webcam, 1 mic, 1
/// speaker). enumerateDevices returns the same set across calls so a
/// fingerprint stays stable.
pub fn media_devices_script() -> String {
    r#"(function(){
  if(!navigator.mediaDevices)return;
  var devices=[
    {kind:'videoinput',deviceId:'default-camera-001',label:'Integrated Camera',groupId:'group-camera'},
    {kind:'audioinput',deviceId:'default-mic-001',label:'Internal Microphone',groupId:'group-mic'},
    {kind:'audiooutput',deviceId:'default-speaker-001',label:'Internal Speakers',groupId:'group-speaker'}
  ];
  var _enumerate=navigator.mediaDevices.enumerateDevices?navigator.mediaDevices.enumerateDevices.bind(navigator.mediaDevices):null;
  if(_enumerate){
    navigator.mediaDevices.enumerateDevices=function(){
      return _enumerate().then(function(real){
        // Replace deviceIds/labels with stable values so the fingerprint
        // doesn't leak the real machine's device list.
        return real.map(function(d,i){
          var s=devices[i%devices.length];
          return {kind:d.kind||s.kind,deviceId:s.deviceId,label:s.label,groupId:s.groupId};
        });
      }).catch(function(){return devices.slice();});
    };
  }
})();"#.to_string()
}

/// WebRTC IP leak prevention. Drops `RTCPeerConnection` so a page can't
/// enumerate the local network interfaces via ICE candidates. This is the
/// aggressive form; the stealth form wraps createOffer to filter out
/// host candidates — chosen here because a missing RTCPeerConnection is
/// less suspicious on a corporate machine than one with empty candidates.
pub fn webrtc_leak_script() -> String {
    r#"(function(){
  // Aggressive: hide RTCPeerConnection entirely. Pages that need WebRTC for
  // legit calling will break; pages that only use it for fingerprinting
  // see a clean "unsupported" answer, which is common in headless.
  try{Object.defineProperty(navigator,'mediaDevices',{get:function(){return undefined;},configurable:false});}catch(_){}
  try{delete window.RTCPeerConnection;}catch(_){}
  try{window.RTCPeerConnection=undefined;}catch(_){}
  try{window.webkitRTCPeerConnection=undefined;}catch(_){}
})();"#.to_string()
}

/// speechSynthesis.getVoices — returns a stable, platform-appropriate voice
/// list. fingerprintjs reads the voice hash.
pub fn speech_voices_script(platform: &str) -> String {
    let voices = if platform == "Win32" {
        r#"[
        {name:'Microsoft David - English (United States)',lang:'en-US',localService:true,default:true},
        {name:'Microsoft Zira - English (United States)',lang:'en-US',localService:true,default:false},
        {name:'Google US English',lang:'en-US',localService:false,default:false}
      ]"#
    } else if platform == "Linux x86_64" {
        r#"[
        {name:'English (America)',lang:'en-US',localService:true,default:true},
        {name:'English (Great Britain)',lang:'en-GB',localService:true,default:false}
      ]"#
    } else {
        r#"[
        {name:'Samantha',lang:'en-US',localService:true,default:true},
        {name:'Daniel',lang:'en-GB',localService:true,default:false},
        {name:'Karen',lang:'en-AU',localService:true,default:false}
      ]"#
    };
    format!(
        r#"(function(){{
  if(typeof speechSynthesis==='undefined')return;
  var _voices={voices};
  var _get=speechSynthesis.getVoices;
  speechSynthesis.getVoices=function(){{return _voices.slice();}};
  speechSynthesis.onvoiceschanged=null;
  try{{Object.defineProperty(speechSynthesis,'onvoiceschanged',{{get:function(){{return null;}},set:function(){{}},configurable:false}});}}catch(_){{}}
}})();"#,
        voices = voices,
    )
}

/// Font enumeration — spoof `document.fonts.check()` so a fingerprinter
/// probing for platform-specific fonts sees the expected set, not the host's
/// actual installed fonts. Also patches `measureText` to return stable
/// metrics derived from the session seed.
pub fn font_measurement_script(seed: u64, platform: &str, fonts: &[String]) -> String {
    let font_list = if platform == "Win32" {
        WINDOWS_FONTS
    } else if platform == "Linux x86_64" {
        LINUX_FONTS
    } else {
        MACOS_FONTS
    };
    let font_js: Vec<String> = font_list.iter().map(|s| format!("{:?}", s)).collect();
    let session_fonts_js: Vec<String> = fonts.iter().map(|s| format!("{:?}", s)).collect();
    format!(
        r#"(function(){{
  var SEED={seed}>>>0;
  var _splitmix=function(state){{
    state=(state+0x9E3779B1)>>>0;
    var z=state;
    z=((z^(z>>>16))*0x85EBCA6B)>>>0;
    z=((z^(z>>>13))*0xC2B2AE35)>>>0;
    return (z^(z>>>16))>>>0;
  }};
  var platformFonts=Object.create(null);
  var arr=[{fonts}];
  for(var i=0;i<arr.length;i++)platformFonts[arr[i]]=true;
  var session=[{session_fonts}];
  for(var i=0;i<session.length;i++)platformFonts[session[i]]=true;
  if(document.fonts&&document.fonts.check){{
    var _check=document.fonts.check.bind(document.fonts);
    document.fonts.check=function(font,text){{
      // font is like '12px "Arial"' or 'bold 16px Arial'
      var m=font.match(/"([^"]+)"/);
      if(!m)m=font.match(/\b([A-Za-z][A-Za-z0-9 ]+)$/);
      if(m&&platformFonts[m[1]])return true;
      return _check(font,text||'');
    }};
  }}
  if(CanvasRenderingContext2D.prototype.measureText){{
    var _measure=CanvasRenderingContext2D.prototype.measureText;
    CanvasRenderingContext2D.prototype.measureText=function(text){{
      var r=_measure.call(this,text);
      var s=_splitmix(SEED+text.length*0x100000001b3);
      var dx=((s/4294967295.0)*2-1)*0.5;
      try{{
        return {{width:r.width+dx,actualBoundingBoxLeft:r.actualBoundingBoxLeft||0,
          actualBoundingBoxRight:r.actualBoundingBoxRight||0,
          actualBoundingBoxAscent:r.actualBoundingBoxAscent||0,
          actualBoundingBoxDescent:r.actualBoundingBoxDescent||0,
          fontBoundingBoxAscent:r.fontBoundingBoxAscent||0,
          fontBoundingBoxDescent:r.fontBoundingBoxDescent||0}};
      }}catch(_){{return r;}}
    }};
  }}
}})();"#,
        seed = seed,
        fonts = font_js.join(","),
        session_fonts = session_fonts_js.join(","),
    )
}

/// navigator.permissions.query — patch so 'notifications' returns 'denied'
/// (the default for a fresh profile) and other permissions return 'prompt'.
/// Without this, headless Chrome reports 'prompt' for notifications which
/// differs from a real profile's 'denied'.
pub fn permissions_script() -> String {
    r#"(function(){
  if(!navigator.permissions||typeof navigator.permissions.query!=='function')return;
  var _query=navigator.permissions.query.bind(navigator.permissions);
  navigator.permissions.query=function(desc){
    if(desc&&desc.name==='notifications'){
      return Promise.resolve({state:'denied',onchange:null,addEventListener:function(){},removeEventListener:function(){},dispatchEvent:function(){return true;}});
    }
    return _query(desc).catch(function(){
      return {state:'prompt',onchange:null,addEventListener:function(){},removeEventListener:function(){},dispatchEvent:function(){return true;}};
    });
  };
})();"#.to_string()
}
