//! KVM launch page / built-in viewer — Phase 7 (docs/46).
//!
//! A self-contained browser viewer (served from flash) speaking RFB 3.8 with Raw
//! encoding over `wss://` — a zero-dependency stand-in for noVNC until its
//! minified bundle is vendored (asset step; MPL-2.0). Raw keeps the client tiny
//! and correct; the server (rfb.rs) falls back to Raw when only Raw is offered.
//!
//! Provenance: RFC 6143 (client side, clean-room), docs/46 (decision). No HP code.

#![allow(dead_code)]

/// Launch page (HTML + JS RFB/Raw client). Served at `/html/novnc/vnc.html`.
pub const LAUNCH_PAGE: &str = r##"<!doctype html>
<html><head><meta charset="utf-8"><title>ilo3-rs KVM</title></head>
<body style="background:#000;margin:0;color:#fff;font:12px monospace">
<div id="s"></div><canvas id="c"></canvas>
<script>
'use strict';
const cv=document.getElementById('c'), cx=cv.getContext('2d'), st=document.getElementById('s');
let W=0,H=0,img=null,phase=0,rbuf=[],sock=null;
const U16=(b,o)=>(b[o]<<8)|b[o+1];
const U32=(b,o)=>((b[o]<<24|b[o+1]<<16|b[o+2]<<8|b[o+3])>>>0);
function send(a){sock.send(a);}
function sendbytes(b){sock.send(new Uint8Array(b));}
function setPx(x,y,v){
  if(x>=W||y>=H)return; const p=(y*W+x)*4;
  const r=(v>>11)&31,g=(v>>5)&63,b=v&31;
  img.data[p]=r<<3;img.data[p+1]=g<<2;img.data[p+2]=b<<3;img.data[p+3]=255;
}
function rawRect(x,y,w,h,o){ for(let yy=0;yy<h;yy++)for(let xx=0;xx<w;xx++){ setPx(x+xx,y+yy,(rbuf[o]|(rbuf[o+1]<<8)));o+=2; } }
function parseFb(){
  let pos=1; const n=(rbuf[pos]<<8)|rbuf[pos+1]; pos+=2;
  for(let i=0;i<n;i++){
    if(rbuf.length<pos+12)return false;
    const x=U16(rbuf,pos),y=U16(rbuf,pos+2),w=U16(rbuf,pos+4),h=U16(rbuf,pos+6); pos+=8;
    const enc=(rbuf[pos]<<24|rbuf[pos+1]<<16|rbuf[pos+2]<<8|rbuf[pos+3])>>>0; pos+=4;
    if(enc===0){ if(rbuf.length<pos+w*h*2)return false; rawRect(x,y,w,h,pos); pos+=w*h*2; }
    else return false;
  }
  rbuf.splice(0,pos); return true;
}
function reqfull(){
  const u=[3,0,0,0,0,0,W>>8,W&255,H>>8,H&255];
  sendbytes(u);
}
function step(){
  if(phase===0){ if(rbuf.length<12)return; rbuf.splice(0,12); phase=1; }
  else if(phase===1){ if(rbuf.length<1)return; const n=rbuf.shift(); if(rbuf.length<n)return; rbuf.splice(0,n); sendbytes([1]); phase=2; }
  else if(phase===2){ if(rbuf.length<4)return; rbuf.splice(0,4); sendbytes([1]); phase=3; }
  else if(phase===3){
    if(rbuf.length<24)return; W=U16(rbuf,0); H=U16(rbuf,2); const nn=U32(rbuf,20);
    if(rbuf.length<24+nn)return; rbuf.splice(0,24+nn);
    cv.width=W; cv.height=H; img=cx.createImageData(W,H);
    sendbytes([2,0,0,1,0,0,0,0]); reqfull(); phase=4;
  }
  else if(phase===4){
    while(rbuf.length>0){
      const t=rbuf[0];
      if(t===0){ if(!parseFb())break; cx.putImageData(img,0,0); }
      else if(t===2){ rbuf.splice(0,1); }
      else { rbuf.splice(0,1); }
    }
  }
}
function connect(){
  sock=new WebSocket('wss://'+location.hostname+':6080/');
  sock.binaryType='arraybuffer';
  sock.onopen=()=>{ const v='RFB 003.008\n'; const b=new Uint8Array(12); for(let i=0;i<12;i++)b[i]=v.charCodeAt(i); sendbytes(b); st.textContent='connected'; };
  sock.onmessage=e=>{ const a=new Uint8Array(e.data); for(let i=0;i<a.length;i++)rbuf.push(a[i]); step(); };
  sock.onerror=()=>st.textContent='websocket error';
  sock.onclose=()=>st.textContent='closed';
}
connect();
</script></body></html>
"##;
