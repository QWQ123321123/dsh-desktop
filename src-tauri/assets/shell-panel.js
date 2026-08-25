// In-page chrome injected into every page of the shell's webview by
// src-tauri/src/lib.rs (initialization_script + on_page_load fallback).
// The shell substitutes __DSH_TOKEN__ with a random per-boot token at
// injection time; the control channel (127.0.0.1:3175) rejects requests
// without it. Syntax-checked via `node --check` in build.rs.
(() => {
  // Runs twice per page (initialization_script + on_page_load fallback);
  // guard everything once-per-page-load.
  if (window.__dshShellBooted) return;
  window.__dshShellBooted = true;
  const API = 'http://127.0.0.1:3175';
  // Origin detection is subtle: in dev, wry serves frontendDist from a random
  // 127.0.0.1 port, so a bare loopback regex misfires on the splash page.
  // Identify splash by path, the dsh UI by its known port range.
  const pagePort = Number(location.port);
  const onDsh = location.hostname === '127.0.0.1' && pagePort >= 3177 && pagePort <= 3186;
  const onSplash = location.pathname.endsWith('splash.html');
  const TOKEN = '__DSH_TOKEN__'; // replaced by the shell at injection time
  function api(p) {
    return fetch(API + p + (p.includes('?') ? '&' : '?') + 't=' + encodeURIComponent(TOKEN));
  }
  function cmd(p) { return api(p).catch(() => {}); }
  function clickDsh(...texts) {
    [...document.querySelectorAll('button')].find(b => texts.includes(b.textContent.trim()))?.click();
  }

  // ---------------- custom titlebar (Codex-style chrome) ----------------
  const MENUS = [
    { label: '文件', items: [
      { label: '新建会话', key: 'Ctrl+N', act: () => clickDsh('新会话', 'New Session') },
      { label: '设置', key: 'Ctrl+,', act: () => clickDsh('设置', 'Settings') },
      '-',
      { label: '退出', act: () => cmd('/win/quit') },
    ]},
    { label: '编辑', items: [
      // 只留全选：剪贴/复制/粘贴在 React 受控输入框上靠 execCommand 必然失效，
      // 原生右键菜单（M3 放行后）已覆盖这些编辑操作。
      { label: '全选', key: 'Ctrl+A', act: () => document.execCommand('selectAll') },
    ]},
    { label: '视图', items: [
      { label: '刷新界面', key: 'Ctrl+R', act: () => location.reload() },
      { label: '开发者工具', key: 'F12', act: () => cmd('/win/devtools') },
      // 不提供全屏：无边框窗口 set_fullscreen 换 WS_POPUP 样式后还原不干净，
      // 会导致边框缩放失效；最大化已覆盖该场景。
    ]},
    { label: '帮助', items: [
      { label: '检查更新…', act: () => updateFlow() },
      '-',
      { label: '关于 DeepSeek Harness', act: () => cmd('/win/about') },
    ]},
  ];

  function buildTitlebar() {
    if (document.getElementById('dsh-shell-titlebar')) return;
    const host = document.createElement('div');
    host.id = 'dsh-shell-titlebar';
    const shadow = host.attachShadow({ mode: 'open' });
    shadow.innerHTML = `
      <style>
        .bar { position:fixed; top:0; left:0; right:0; height:32px; z-index:2147483646;
               display:flex; align-items:center; justify-content:space-between;
               font:13px/1 "Segoe UI","Microsoft YaHei",sans-serif; user-select:none;
               color: light-dark(#1f2328, #e8eaed);
               text-shadow: 0 1px 3px light-dark(rgba(255,255,255,.5), rgba(0,0,0,.5));
               /* transparent fill: the page background image must show through
                  (a filled bar over the margin-top strip caused a visible seam);
                  backdrop blur alone keeps labels readable over any image */
               backdrop-filter: blur(12px); -webkit-backdrop-filter: blur(12px); }
        .left { display:flex; align-items:center; height:100%; }
        .title { padding:0 10px; opacity:.6; font-size:12px; }
        .mbtn { padding:0 10px; height:100%; display:flex; align-items:center; cursor:default; border-radius:4px;
                transition: background .15s ease, transform .08s ease; }
        .mbtn:hover { background:rgba(127,127,127,.18); }
        .mbtn:active { transform: scale(.94); }
        .drop { position:absolute; top:32px; min-width:210px; padding:4px; border-radius:8px;
                background: light-dark(rgba(252,252,252,.97), rgba(42,42,45,.97));
                box-shadow:0 6px 20px rgba(0,0,0,.28);
                animation: dshDropIn .14s ease-out; }
        @keyframes dshDropIn { from { opacity: 0; transform: translateY(-4px); } to { opacity: 1; transform: none; } }
        .mi { display:flex; justify-content:space-between; gap:24px; padding:6px 12px; border-radius:5px; cursor:default;
              transition: background .12s ease; }
        .mi:hover { background:rgba(77,107,254,.18); }
        .mi:active { transform: scale(.98); }
        .sep { height:1px; margin:4px 8px; background:rgba(127,127,127,.25); }
        .key { opacity:.5; font-size:11px; }
        .right { display:flex; height:100%; }
        .wbtn { width:44px; display:flex; align-items:center; justify-content:center; font-size:12px;
                transition: background .15s ease, color .15s ease; }
        .wbtn:hover { background:rgba(127,127,127,.18); }
        .wbtn.close:hover { background:#e81123; color:#fff; }
      </style>
      <div class="bar">
        <div class="left"><span class="title">DeepSeek Harness</span></div>
        <div class="right">
          <div class="wbtn" id="wmin">—</div>
          <div class="wbtn" id="wmax">▢</div>
          <div class="wbtn close" id="wclose">✕</div>
        </div>
      </div>`;
    document.documentElement.appendChild(host);
    const bar = shadow.querySelector('.bar');
    const left = shadow.querySelector('.left');
    let openDrop = null;
    const closeDrop = () => { openDrop?.remove(); openDrop = null; };
    // Shadow DOM retargets event.target to the host for document-level
    // listeners, so watch BOTH levels: inside the shadow root (real targets)
    // for presses on the bar outside the dropdown, and at the document for
    // presses anywhere else in the page.
    shadow.addEventListener('mousedown', (e) => {
      if (openDrop && !openDrop.contains(e.target)) closeDrop();
    });
    document.addEventListener('mousedown', (e) => {
      if (openDrop && e.target !== host) closeDrop();
    });
    for (const m of MENUS) {
      const btn = document.createElement('div');
      btn.className = 'mbtn';
      btn.textContent = m.label;
      btn.onmousedown = e => e.stopPropagation();
      btn.onclick = () => {
        if (openDrop) { closeDrop(); return; }
        const drop = document.createElement('div');
        drop.className = 'drop';
        drop.style.left = btn.offsetLeft + 'px';
        for (const it of m.items) {
          if (it === '-') {
            const s = document.createElement('div');
            s.className = 'sep';
            drop.appendChild(s);
            continue;
          }
          const row = document.createElement('div');
          row.className = 'mi';
          const l = document.createElement('span'); l.textContent = it.label;
          const k = document.createElement('span'); k.className = 'key'; k.textContent = it.key ?? '';
          row.append(l, k);
          row.onclick = () => { closeDrop(); it.act(); };
          drop.appendChild(row);
        }
        bar.appendChild(drop);
        openDrop = drop;
      };
      left.appendChild(btn);
    }
    bar.onmousedown = e => {
      if (e.target === bar || e.target === left || e.target.classList?.contains('title')) cmd('/win/drag');
    };
    bar.ondblclick = e => { if (e.target === bar || e.target === left) cmd('/win/max'); };
    shadow.getElementById('wmin').onclick = () => cmd('/win/min');
    shadow.getElementById('wmax').onclick = () => cmd('/win/max');
    shadow.getElementById('wclose').onclick = () => cmd('/win/close');
  }

  // Reserve the titlebar strip; dsh full-height layouts tolerate this because
  // their scroll containers are internal flex children. Also hosts the global
  // page-entry animation (subtle fade/slide on every load) with a
  // reduced-motion escape hatch.
  function installChromeStyle() {
    if (document.getElementById('dsh-shell-chrome')) return;
    const style = document.createElement('style');
    style.id = 'dsh-shell-chrome';
    style.textContent = `
      html { margin-top: 32px !important; height: calc(100% - 32px) !important; overflow: hidden !important; }
      @keyframes dshPageIn { from { opacity: 0; transform: translateY(6px); } to { opacity: 1; transform: none; } }
      @media (prefers-reduced-motion: no-preference) {
        body { animation: dshPageIn .28s ease-out; }
      }`;
    document.documentElement.appendChild(style);
  }

  document.addEventListener('keydown', e => {
    if (e.ctrlKey && !e.shiftKey && (e.key === 'n' || e.key === 'N')) { e.preventDefault(); clickDsh('新会话', 'New Session'); }
    if (e.ctrlKey && e.key === ',') { e.preventDefault(); clickDsh('设置', 'Settings'); }
    if (e.key === 'F12') { e.preventDefault(); cmd('/win/devtools'); }
  });

  // Block the browser chrome context menu (back/forward/reload — "back"
  // strands the window on the splash page); keep it on text fields for
  // editing actions.
  document.addEventListener('contextmenu', e => {
    const t = e.target;
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return;
    e.preventDefault();
  });

  // Splash self-heals: poll the control channel for the ready URL instead of
  // relying on the boot thread's one-shot navigate(). Fade out before leaving
  // so the splash → UI switch reads as a transition, not a flash.
  if (onSplash) {
    const poll = setInterval(async () => {
      try {
        const r = await (await api('/win/state')).json();
        if (r.url) {
          clearInterval(poll);
          document.body.style.transition = 'opacity .25s ease';
          document.body.style.opacity = '0';
          setTimeout(() => location.replace(r.url), 260);
        }
      } catch (e) { /* shell not up yet */ }
    }, 800);
  }

  // ---------------- background panel (dsh UI origin only) ----------------
  let panel, slider, overlay;
  async function applyState() {
    try {
      const s = await (await api('/bg/state')).json();
      document.getElementById('dsh-shell-bg')?.remove();
      overlay = null;
      if (s.image) {
        overlay = document.createElement('div');
        overlay.id = 'dsh-shell-bg';
        overlay.style.cssText = 'position:fixed;inset:0;z-index:2147483645;pointer-events:none;opacity:' + s.opacity + ';background:url(' + s.image + ') center/cover no-repeat;';
        document.documentElement.appendChild(overlay);
      }
      if (slider) slider.value = Math.round(s.opacity * 100);
    } catch (e) { /* shell control channel down: stay silent */ }
  }
  window.__dshBgReload = applyState;
  function buildPanel() {
    if (document.getElementById('dsh-shell-bg-panel')) return;
    const host = document.createElement('div');
    host.id = 'dsh-shell-bg-panel';
    host.style.cssText = 'position:fixed;right:16px;bottom:16px;z-index:2147483647;';
    const shadow = host.attachShadow({ mode: 'open' });
    shadow.innerHTML = `
      <style>
        /* light-dark() follows dsh's own theme toggle: dsh sets
           color-scheme on the root element, which inherits into shadow DOM. */
        .fab { width:36px;height:36px;border-radius:50%;border:1px solid light-dark(#dde1e7,#44464c);
               background: light-dark(#fff,#2a2a2d);
               cursor:pointer;font-size:16px;line-height:1;box-shadow:0 2px 8px rgba(0,0,0,.12); }
        .pop { display:none;position:absolute;bottom:44px;right:0;
               background: light-dark(#fff,#2a2a2d);border:1px solid light-dark(#dde1e7,#44464c);
               border-radius:10px;padding:12px;width:190px;box-shadow:0 4px 16px rgba(0,0,0,.15);
               font:12px/1.6 "Segoe UI","Microsoft YaHei",sans-serif;color: light-dark(#1f2328,#e8eaed); }
        .pop.open { display:block; }
        button.act { width:100%;margin:2px 0;padding:5px 8px;border:1px solid light-dark(#dde1e7,#44464c);
                     border-radius:6px;background: light-dark(#f7f8fa,#3a3d44);cursor:pointer;font-size:12px;
                     color: light-dark(#1f2328,#e8eaed); transition: background .12s ease, transform .08s ease; }
        button.act:hover { background: light-dark(#eef1f6,#484b54); }
        button.act:active { transform: scale(.97); }
        .fab { transition: transform .12s ease, box-shadow .12s ease; }
        .fab:hover { transform: scale(1.08); box-shadow:0 4px 12px rgba(0,0,0,.2); }
        .fab:active { transform: scale(.94); }
        .pop { animation: dshPopIn .16s ease-out; }
        @keyframes dshPopIn { from { opacity: 0; transform: translateY(6px) scale(.96); } to { opacity: 1; transform: none; } }
        input[type=range] { width:100%;margin-top:6px; }
        .row { margin-top:8px;color: light-dark(#8a919c,#9aa0a8); }
      </style>
      <button class="fab" title="背景设置">🎨</button>
      <div class="pop">
        <button class="act" id="pick">选择背景图片…</button>
        <button class="act" id="clear">清除背景</button>
        <div class="row">透明度 <span id="val"></span>%</div>
        <input type="range" id="op" min="2" max="60" value="18">
      </div>`;
    document.documentElement.appendChild(host);
    panel = shadow.querySelector('.pop');
    slider = shadow.getElementById('op');
    const val = shadow.getElementById('val');
    shadow.querySelector('.fab').onclick = () => panel.classList.toggle('open');
    shadow.getElementById('pick').onclick = async () => { await api('/bg/pick'); applyState(); };
    shadow.getElementById('clear').onclick = async () => { await api('/bg/clear'); applyState(); };
    let t;
    slider.oninput = () => {
      val.textContent = slider.value;
      if (overlay) overlay.style.opacity = slider.value / 100;
      clearTimeout(t);
      t = setTimeout(() => api('/bg/opacity?v=' + slider.value / 100), 300);
    };
    slider.oninput();
  }
  // dsh 设置弹窗 → 整页：SettingsRoot 的结构是 overlay > mask + panel(nav)，
  // 用结构选择器命中（CSS Modules 的类名带哈希，不能硬编码）。
  // top 留出 32px 标题栏，窗口控件保持可用。
  function installSettingsPageStyle() {
    if (document.getElementById('dsh-shell-settings-page')) return;
    const style = document.createElement('style');
    style.id = 'dsh-shell-settings-page';
    style.textContent = `
      [role="presentation"]:has(> [role="dialog"][aria-modal="true"] nav) { position: fixed; inset: 0; }
      [role="presentation"]:has(> [role="dialog"][aria-modal="true"] nav) > [aria-hidden="true"] { display: none; }
      [role="dialog"][aria-modal="true"]:has(nav) {
        position: fixed !important; inset: 32px 0 0 0 !important;
        width: 100vw !important; height: calc(100vh - 32px) !important;
        max-width: none !important; max-height: none !important;
        margin: 0 !important; border-radius: 0 !important; border: none !important;
      }
      @media (prefers-reduced-motion: no-preference) {
        [role="dialog"][aria-modal="true"]:has(nav) { animation: dshPageIn .22s ease-out; }
      }`;
    document.documentElement.appendChild(style);
  }

  // ---------------- self-update dialog (帮助 → 检查更新) ----------------
  // The flow rides the control channel: check → start (background download
  // on the Rust side) → poll status → install (spawn installer + exit).
  function esc(s) {
    const d = document.createElement('div');
    d.textContent = String(s);
    return d.innerHTML;
  }
  function fmtSize(n) {
    if (!n) return '';
    const mb = n / 1048576;
    return mb >= 1 ? mb.toFixed(1) + ' MB' : Math.max(1, Math.round(n / 1024)) + ' KB';
  }
  let updateHost = null;
  function showUpdate(html, buttons) {
    if (!updateHost) {
      updateHost = document.createElement('div');
      updateHost.id = 'dsh-shell-update';
      document.documentElement.appendChild(updateHost);
    }
    const shadow = updateHost.shadowRoot || updateHost.attachShadow({ mode: 'open' });
    const btnHtml = buttons.map(b => `<button class="b${b.primary ? ' primary' : ''}" data-b="${b.id}">${b.label}</button>`).join('');
    shadow.innerHTML = `
      <style>
        .mask { position:fixed; inset:0; z-index:2147483647; display:flex; align-items:center; justify-content:center;
                background:rgba(0,0,0,.35); font:13px/1.6 "Segoe UI","Microsoft YaHei",sans-serif;
                color: light-dark(#1f2328,#e8eaed); }
        .card { width:440px; max-width:calc(100vw - 48px); border-radius:12px; padding:18px 20px;
                background: light-dark(#fff,#2a2a2d); box-shadow:0 8px 32px rgba(0,0,0,.35);
                animation: dshPopIn .18s ease-out; }
        .t { font-size:15px; font-weight:600; margin-bottom:10px; }
        .notes { max-height:180px; overflow:auto; white-space:pre-wrap; word-break:break-word;
                 font-size:12px; opacity:.85; border:1px solid light-dark(#dde1e7,#3a3d44);
                 border-radius:8px; padding:8px 10px; margin-bottom:6px; }
        .meta { font-size:12px; opacity:.7; margin-bottom:12px; }
        .bar { height:6px; border-radius:3px; background: light-dark(#e8ebef,#3a3d44); overflow:hidden; margin:10px 0 4px; }
        .bar i { display:block; height:100%; width:0%; background:#4d7dfe; transition:width .25s ease; }
        .pct { font-size:12px; opacity:.75; margin-bottom:12px; }
        .btns { display:flex; justify-content:flex-end; gap:8px; margin-top:14px; }
        button.b { padding:6px 14px; border-radius:7px; border:1px solid light-dark(#dde1e7,#44464c);
                   background: light-dark(#f7f8fa,#3a3d44); cursor:pointer; font-size:13px;
                   color: inherit; transition: background .12s ease, transform .08s ease; }
        button.b:hover { background: light-dark(#eef1f6,#484b54); }
        button.b:active { transform: scale(.97); }
        button.b.primary { background:#4d7dfe; border-color:#4d7dfe; color:#fff; }
        button.b.primary:hover { background:#3f6ce8; }
      </style>
      <div class="mask"><div class="card">${html}<div class="btns">${btnHtml}</div></div></div>`;
    for (const b of buttons) {
      shadow.querySelector(`[data-b="${b.id}"]`).onclick = () => b.act(shadow);
    }
    return shadow;
  }
  function closeUpdate() { updateHost?.remove(); updateHost = null; }

  const updateButtons = {
    dl: () => { cmd('/win/update/open'); closeUpdate(); },
    ok: () => closeUpdate(),
    retry: () => updateFlow(),
  };

  async function updateFlow() {
    showUpdate(`<div class="t">正在检查更新…</div>`,
      [{ id: 'ok', label: '关闭', act: updateButtons.ok }]);
    let r;
    try {
      r = await (await api('/win/update/check')).json();
    } catch (e) {
      showUpdate(`<div class="t">检查更新失败</div><div class="meta">无法连接更新服务。请检查网络后重试，或从下载页手动安装。</div>`,
        [{ id: 'dl', label: '打开下载页', act: updateButtons.dl },
         { id: 'retry', label: '重试', act: updateButtons.retry },
         { id: 'ok', label: '关闭', act: updateButtons.ok }]);
      return;
    }
    if (!r.ok) {
      showUpdate(`<div class="t">检查更新失败</div><div class="meta">${esc(r.error || '未知错误')}</div>`,
        [{ id: 'dl', label: '打开下载页', act: updateButtons.dl },
         { id: 'ok', label: '关闭', act: updateButtons.ok }]);
      return;
    }
    if (!r.has_update) {
      showUpdate(`<div class="t">已是最新版本</div><div class="meta">当前版本 v${esc(r.current)}</div>`,
        [{ id: 'ok', label: '关闭', act: updateButtons.ok }]);
      return;
    }
    const notes = (r.notes || '').trim();
    showUpdate(
      `<div class="t">发现新版本 v${esc(r.version)}</div>` +
      (notes ? `<div class="notes">${esc(notes)}</div>` : '') +
      `<div class="meta">安装包大小 ${fmtSize(r.size)} · 当前版本 v${esc(r.current)}</div>`,
      [{ id: 'go', label: '立即更新', primary: true, act: () => startUpdate() },
       { id: 'dl', label: '打开下载页', act: updateButtons.dl },
       { id: 'ok', label: '稍后', act: updateButtons.ok }]);
  }

  async function startUpdate() {
    // A previous round may have finished downloading already — surface the
    // install button directly instead of re-downloading.
    let st;
    try { st = await (await api('/win/update/status')).json(); } catch (e) { /* poll handles it */ }
    if (st && st.ok && st.state === 'ready') { await pollUpdate(); return; }
    showUpdate(`<div class="t">正在下载更新…</div><div class="bar"><i></i></div><div class="pct"></div>`,
      [{ id: 'ok', label: '后台继续', act: updateButtons.ok }]);
    try { await (await api('/win/update/start')).json(); } catch (e) { /* poll will surface the error */ }
    await pollUpdate();
  }

  function pollUpdate() {
    return new Promise(resolve => {
      const poll = setInterval(async () => {
        let r;
        try { r = await (await api('/win/update/status')).json(); } catch (e) { return; }
        if (!r || !r.ok) return;
        if (r.state === 'downloading') {
          const pct = r.total ? Math.min(100, Math.round(r.downloaded / r.total * 100)) : 0;
          showUpdate(`<div class="t">正在下载更新…</div><div class="bar"><i style="width:${pct}%"></i></div><div class="pct">${pct}% · ${fmtSize(r.downloaded)} / ${fmtSize(r.total)}</div>`,
            [{ id: 'ok', label: '后台继续', act: updateButtons.ok }]);
        } else if (r.state === 'verifying') {
          showUpdate(`<div class="t">正在校验安装包…</div>`,
            [{ id: 'ok', label: '后台继续', act: updateButtons.ok }]);
        } else if (r.state === 'ready') {
          clearInterval(poll);
          showUpdate(`<div class="t">更新已就绪</div><div class="meta">应用将退出并自动安装，安装完成后请重新打开。</div>`,
            [{ id: 'go', label: '立即安装', primary: true, act: () => cmd('/win/update/install') },
             { id: 'ok', label: '稍后', act: updateButtons.ok }]);
          resolve();
        } else if (r.state === 'error') {
          clearInterval(poll);
          showUpdate(`<div class="t">下载失败</div><div class="meta">${esc(r.error || '未知错误')}</div>`,
            [{ id: 'dl', label: '打开下载页', act: updateButtons.dl },
             { id: 'retry', label: '重试', act: () => startUpdate() },
             { id: 'ok', label: '关闭', act: updateButtons.ok }]);
          resolve();
        }
      }, 600);
    });
  }
  window.__dshCheckUpdate = updateFlow;

  // ---------------- 语音输入（按住说话，仅 dsh 页面） ----------------
  // 按住 🎤 → /speech/start（WinRT 连续识别）；松开 → /speech/stop 取文本，
  // 注入 dsh 的受控输入框（native setter + input 事件绕过 React 检查）。
  let speechUi = null;
  function findComposer() {
    return document.querySelector('main textarea')
        || document.querySelector('textarea')
        || document.querySelector('[contenteditable="true"]');
  }
  function injectSpeech(text) {
    const el = findComposer();
    if (!el) return false;
    if (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT') {
      const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      const setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
      const cur = el.value || '';
      const next = (cur ? cur.replace(/\s+$/, '') + ' ' : '') + text;
      setter.call(el, next);
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.focus();
    } else {
      el.focus();
      document.execCommand('insertText', false, (el.textContent ? ' ' : '') + text);
    }
    return true;
  }
  function buildSpeechButton() {
    if (speechUi || !onDsh) return;
    const host = document.createElement('div');
    host.id = 'dsh-shell-speech';
    const shadow = host.attachShadow({ mode: 'open' });
    shadow.innerHTML = `
      <style>
        .wrap { position:fixed; right:16px; bottom:64px; z-index:2147483647;
                display:flex; flex-direction:column; align-items:center; gap:6px; }
        .mic { width:44px; height:44px; border-radius:50%; border:1px solid light-dark(#dde1e7,#44464c);
               background: light-dark(#fff,#2a2a2d); cursor:pointer; font-size:18px; line-height:1;
               box-shadow:0 2px 8px rgba(0,0,0,.12); transition: transform .12s ease, background .15s ease; }
        .mic:hover { transform: scale(1.08); }
        .mic:active { transform: scale(.94); }
        .mic.live { background:#e81123; border-color:#e81123;
                    animation: dshMicPulse 1.2s ease-in-out infinite; }
        @keyframes dshMicPulse { 0%,100% { box-shadow:0 0 0 0 rgba(232,17,35,.4); }
                                 50% { box-shadow:0 0 0 10px rgba(232,17,35,0); } }
        .tip { display:none; max-width:220px; padding:4px 10px; border-radius:6px;
               font:12px/1.5 "Segoe UI","Microsoft YaHei",sans-serif; text-align:center;
               color: light-dark(#1f2328,#e8eaed); background: light-dark(rgba(252,252,252,.97),rgba(42,42,45,.97));
               border:1px solid light-dark(#dde1e7,#44464c); box-shadow:0 2px 10px rgba(0,0,0,.18); }
        .tip.show { display:block; }
      </style>
      <div class="wrap">
        <div class="tip">按住说话，松开后输入到对话框</div>
        <button class="mic" title="语音输入（按住说话）">🎤</button>
      </div>`;
    document.documentElement.appendChild(host);
    speechUi = { host, shadow };
    const mic = shadow.querySelector('.mic');
    const tip = shadow.querySelector('.tip');
    let listening = false;
    let pendingStop = false;
    let tipTimer = null;
    function showTip(msg, ms = 2500) {
      tip.textContent = msg;
      tip.classList.add('show');
      clearTimeout(tipTimer);
      tipTimer = setTimeout(() => tip.classList.remove('show'), ms);
    }
    async function beginListen() {
      if (listening) return;
      pendingStop = false;
      let r;
      try { r = await (await api('/speech/start')).json(); }
      catch (e) { showTip('无法连接语音服务'); return; }
      // 快速点按：松手发生在开始完成之前 → 立即停止，识别到多少算多少
      if (pendingStop) {
        try {
          const t = await (await api('/speech/stop')).json();
          if (t.ok && t.text && t.text.trim()) injectSpeech(t.text.trim());
        } catch (e) { /* channel down */ }
        return;
      }
      if (!r.ok) { showTip(r.error || '无法开始识别'); return; }
      listening = true;
      mic.classList.add('live');
    }
    async function endListen() {
      if (!listening) { pendingStop = true; return; }
      listening = false;
      mic.classList.remove('live');
      showTip('正在识别…', 60000);
      let t;
      try { t = await (await api('/speech/stop')).json(); }
      catch (e) { showTip('识别失败'); return; }
      if (t.ok && t.text && t.text.trim()) {
        injectSpeech(t.text.trim()) ? showTip('已输入到对话框') : showTip('未找到输入框');
      } else if (!t.ok) {
        showTip(t.error || '识别失败');
      } else {
        showTip('没有听清，请再试一次');
      }
    }
    mic.addEventListener('mousedown', (e) => { e.preventDefault(); beginListen(); });
    // mouseleave + document mouseup 双保险：按住时拖出按钮/在按钮外松开都能结束
    mic.addEventListener('mouseup', endListen);
    mic.addEventListener('mouseleave', endListen);
    document.addEventListener('mouseup', endListen);
  }

  function init() {
    buildTitlebar();
    installChromeStyle();
    if (onDsh) { buildPanel(); buildSpeechButton(); applyState(); installSettingsPageStyle(); }
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})()
