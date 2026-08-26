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
      { label: '插件市场…', act: () => pluginMarket() },
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

  // ---------------- composer toolbar (语音 + 背景，仅 dsh 页面) ----------------
  // 两个图标按钮作为 dsh 按钮行（发送按钮左侧）的真实兄弟节点注入：行内 flex
  // 布局 + color:inherit 天然跟随 dsh 排版与主题；形状照抄行内按钮的计算样式，
  // 弹层/tip 取 --dsw-alias-* 设计令牌。React 重渲染清掉节点时由
  // MutationObserver 即时补回，600ms 周期兜底（找不到 composer 时自动隐藏）。
  let slider, overlay, toolbarUi = null;
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
  function buildComposerToolbar() {
    if (toolbarUi || !onDsh) return;
    const host = document.createElement('div');
    host.id = 'dsh-shell-toolbar';
    host.style.cssText = 'position:fixed;left:0;top:0;z-index:2147483647;';
    const shadow = host.attachShadow({ mode: 'open' });
    shadow.innerHTML = `
      <style>
        /* 弹层/tip 取色：--tb-* 由 JS 从 dsh 设计令牌（--dsw-alias-*）刷新，
           令牌读不到时退回 light-dark() 主题色 */
        .pop { display:none; position:fixed; width:190px; padding:12px; border-radius:10px; font-size:12px; line-height:1.6;
               background: var(--tb-bg, light-dark(#fff,#2a2a2d));
               border:1px solid var(--tb-bd, light-dark(#dde1e7,#44464c));
               box-shadow:0 4px 16px rgba(0,0,0,.15);
               color: var(--tb-fg, light-dark(#1f2328,#e8eaed));
               animation: tPop .16s ease-out; }
        .pop.open { display:block; }
        @keyframes tPop { from { opacity:0; transform: translateY(6px) scale(.96); }
                          to { opacity:1; transform:none; } }
        .tip { display:none; position:fixed; max-width:280px; padding:4px 10px; border-radius:6px;
               font:12px/1.5 var(--tb-ff, "Segoe UI","Microsoft YaHei",sans-serif); text-align:center;
               background: var(--tb-bg, light-dark(#fff,#2a2a2d));
               border:1px solid var(--tb-bd, light-dark(#dde1e7,#44464c));
               box-shadow:0 2px 10px rgba(0,0,0,.18);
               color: var(--tb-fg, light-dark(#1f2328,#e8eaed)); }
        .tip.show { display:block; }
        button.act { width:100%; margin:2px 0; padding:5px 8px; cursor:pointer; font-size:12px;
                     border:1px solid var(--tb-bd, light-dark(#dde1e7,#44464c)); border-radius:6px;
                     background:transparent; color:inherit;
                     transition: background .12s ease, transform .08s ease; }
        button.act:hover { background: color-mix(in srgb, currentColor 8%, transparent); }
        button.act:active { transform: scale(.97); }
        input[type=range] { width:100%; margin-top:6px; accent-color:#4d7dfe; }
        .row { margin-top:8px; opacity:.7; }
      </style>
      <div class="pop" id="pop">
        <button class="act" id="pick">选择背景图片…</button>
        <button class="act" id="clear">清除背景</button>
        <div class="row">透明度 <span id="val"></span>%</div>
        <input type="range" id="op" min="2" max="60" value="18">
      </div>
      <div class="tip" id="tip"></div>`;
    document.documentElement.appendChild(host);
    // 两个图标按钮是 light-DOM 元素，作为 dsh 按钮行的真实兄弟节点插入
    // （发送按钮左侧）。行内 flex 布局与 color:inherit 让它们天然跟随 dsh 的
    // 排版和主题色；React 重渲染会清掉注入节点，用 MutationObserver 即时补回。
    if (!document.getElementById('dsh-shell-tb-style')) {
      const style = document.createElement('style');
      style.id = 'dsh-shell-tb-style';
      style.textContent = `
        .dsh-tb-btn { display:inline-flex; align-items:center; justify-content:center; flex:none;
                      width:24px; padding:0; border:none; cursor:pointer; background:transparent;
                      color:inherit; height:var(--dsh-tb-h, 28px);
                      border-radius:var(--dsh-tb-r, 6px);
                      transition: background .12s ease, transform .08s ease; }
        .dsh-tb-btn:hover { background: color-mix(in srgb, currentColor 12%, transparent); }
        .dsh-tb-btn:active { transform: scale(.9); }
        .dsh-tb-btn svg { width:14px; height:14px; fill:currentColor; }
        .dsh-tb-btn.live { background: var(--dsh-tb-brand, #4d7dfe); color:#fff;
                           animation: dshTbPulse 1.2s ease-in-out infinite; }
        @keyframes dshTbPulse { 0%,100% { box-shadow:0 0 0 0 rgba(77,125,254,.45); }
                                50% { box-shadow:0 0 0 8px rgba(77,125,254,0); } }
      `;
      document.documentElement.appendChild(style);
    }
    const bgBtn = document.createElement('button');
    bgBtn.className = 'dsh-tb-btn';
    bgBtn.title = '背景设置';
    bgBtn.setAttribute('aria-label', '背景设置');
    bgBtn.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M21 19V5c0-1.1-.9-2-2-2H5c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h14c1.1 0 2-.9 2-2zM8.5 13.5l2.5 3.01L14.5 12l4.5 6H5l3.5-4.5z"/></svg>`;
    const micBtn = document.createElement('button');
    micBtn.className = 'dsh-tb-btn';
    micBtn.title = '语音输入（按住说话）';
    micBtn.setAttribute('aria-label', '语音输入（按住说话）');
    micBtn.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 14c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm5.91-3c-.49 0-.9.36-.98.85C16.52 14.2 14.47 16 12 16s-4.52-1.8-4.93-4.15c-.08-.49-.49-.85-.98-.85-.61 0-1.09.54-1 1.14.49 3 2.89 5.35 5.91 5.78V20c0 .55.45 1 1 1s1-.45 1-1v-2.08c3.02-.43 5.42-2.78 5.91-5.78.1-.6-.39-1.14-1-1.14z"/></svg>`;
    // 定位 dsh 的 composer 根：从 textarea 向上找最近含 _primary（发送）按钮的
    // 容器（CSS Modules 类名带哈希，按后缀匹配；README 已记录这类结构耦合）。
    const locateRow = () => {
      const ta = findComposer();
      let el = ta;
      while (el && el !== document.body) {
        const primary = el.querySelector('button[class$="_primary"]');
        if (primary) return { root: el, row: primary.parentElement, primaryBtn: primary };
        el = el.parentElement;
      }
      return null;
    };
    const placeButtons = (loc) => {
      if (!loc.row.contains(bgBtn)) loc.row.insertBefore(bgBtn, loc.primaryBtn);
      if (!loc.row.contains(micBtn)) loc.row.insertBefore(micBtn, loc.primaryBtn);
      // 形状照抄 dsh 自己的行内按钮；前景色交给 color:inherit 自动跟随主题
      const cs = getComputedStyle(loc.primaryBtn);
      bgBtn.style.setProperty('--dsh-tb-h', cs.height);
      bgBtn.style.setProperty('--dsh-tb-r', cs.borderRadius);
      micBtn.style.setProperty('--dsh-tb-h', cs.height);
      micBtn.style.setProperty('--dsh-tb-r', cs.borderRadius);
      host.style.setProperty('--tb-ff', cs.fontFamily);
      // 弹层取色：从 composer 根解析 dsh 设计令牌，主题切换后周期重读即跟随
      const rs = getComputedStyle(loc.root);
      const tk = (full, tb) => {
        const v = rs.getPropertyValue(full).trim();
        if (v) host.style.setProperty(tb, v);
      };
      tk('--dsw-alias-brand-primary', '--dsh-tb-brand');
      tk('--dsw-alias-bg-layer-1', '--tb-bg');
      tk('--dsw-alias-label-primary', '--tb-fg');
      tk('--dsw-alias-border-l1', '--tb-bd');
    };
    const tip = shadow.getElementById('tip');
    const pop = shadow.getElementById('pop');
    slider = shadow.getElementById('op');
    const val = shadow.getElementById('val');
    const st = { listening: false, pendingStop: false, tipTimer: null };
    toolbarUi = { host, tip, pop, mic: micBtn, st, loc: null };
    // 弹层/tip 锚在 composer 右上角（输入框上方）
    const reanchor = () => {
      const ta = findComposer();
      const r = ta && ta.getBoundingClientRect();
      if (!r || (!r.width && !r.height)) {
        pop.classList.remove('open');
        tip.classList.remove('show');
        return;
      }
      const anchor = (el) => {
        el.style.left = `${Math.round(r.right - el.offsetWidth - 8)}px`;
        el.style.top = `${Math.round(r.top - el.offsetHeight - 8)}px`;
      };
      if (tip.classList.contains('show')) anchor(tip);
      if (pop.classList.contains('open')) anchor(pop);
    };
    window.addEventListener('scroll', reanchor, true);
    window.addEventListener('resize', reanchor);
    // React 重渲染会移除注入的按钮：观察 composer 根，即时补回
    let mo = null;
    let obsTarget = null;
    const armObs = () => {
      const loc = toolbarUi.loc;
      if (!loc || obsTarget === loc.root) return;
      if (mo) mo.disconnect();
      obsTarget = loc.root;
      mo = new MutationObserver(() => {
        const l = locateRow();
        if (!l) return;
        toolbarUi.loc = l;
        placeButtons(l);
      });
      mo.observe(loc.root, { childList: true, subtree: true });
    };
    // 周期兜底：定位失败重试 / 主题令牌刷新 / 弹层跟随布局
    setInterval(() => {
      const loc = locateRow();
      if (loc) {
        toolbarUi.loc = loc;
        placeButtons(loc);
        armObs();
      }
      reanchor();
    }, 600);
    // ---- 语音：按住开始 / 松开取文本 ----
    const showTip = (msg, ms = 2500) => {
      tip.textContent = msg;
      tip.classList.add('show');
      clearTimeout(st.tipTimer);
      st.tipTimer = setTimeout(() => tip.classList.remove('show'), ms);
      reanchor();
    };
    async function beginListen() {
      if (st.listening) return;
      st.pendingStop = false;
      let r;
      try { r = await (await api('/speech/start')).json(); }
      catch (e) { showTip('无法连接语音服务'); return; }
      // 快速点按：松手发生在开始完成之前 → 立即停止，识别到多少算多少
      if (st.pendingStop) {
        try {
          const t = await (await api('/speech/stop')).json();
          if (t.ok && t.text && t.text.trim()) injectSpeech(t.text.trim());
        } catch (e) { /* channel down */ }
        return;
      }
      if (!r.ok) { showTip(r.error || '无法开始识别'); return; }
      if (r.mic_mismatch) {
        // 识别器固定用系统“默认通信设备”的麦克风，与平时常用的默认输入
        // 设备不同（例如蓝牙耳机不在通话模式时麦克风静音），提示一次即可。
        showTip('提示：语音识别使用系统的默认通信设备麦克风，若听不到声音请在 Windows 声音设置中检查');
      }
      st.listening = true;
      micBtn.classList.add('live');
    }
    async function endListen() {
      if (!st.listening) { st.pendingStop = true; return; }
      st.listening = false;
      micBtn.classList.remove('live');
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
    // 指针捕获替代 mouseleave：按钮只有 24px，按住时鼠标略移出按钮就会
    // 触发 mouseleave 误停（识别会话瞬间结束 → 永远识别不出内容）。
    // setPointerCapture 后拖出按钮在任意位置松开，pointerup 仍回到按钮。
    micBtn.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      try { micBtn.setPointerCapture(e.pointerId); } catch (err) { /* 无碍 */ }
      beginListen();
    });
    micBtn.addEventListener('pointerup', endListen);
    micBtn.addEventListener('pointercancel', endListen);
    // 兜底：非指针环境（理论上 WebView2 都有指针事件）或捕获失败时，
    // 全局 mouseup 也能结束监听
    document.addEventListener('mouseup', endListen);
    // ---- 背景：选择图片 / 清除 / 透明度 ----
    bgBtn.onclick = () => { pop.classList.toggle('open'); reanchor(); };
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

  // ---------------- 插件市场（帮助 → 插件市场） ----------------
  // 目录由 GitHub 仓库根目录的 catalog.json 托管（Rust 侧拉取 + 缓存）；
  // 安装/卸载/装 pnpm 都是 /plugin/* 后台任务（dsh plugin = pnpm 转发器，
  // 可能要跑几分钟）。装完/卸完需重启应用生效（/win/restart）。
  let marketHost = null;
  function showMarket(html, buttons) {
    if (!marketHost) {
      marketHost = document.createElement('div');
      marketHost.id = 'dsh-shell-market';
      document.documentElement.appendChild(marketHost);
    }
    const shadow = marketHost.shadowRoot || marketHost.attachShadow({ mode: 'open' });
    const btnHtml = buttons.map(b => `<button class="b${b.primary ? ' primary' : ''}" data-b="${b.id}">${b.label}</button>`).join('');
    shadow.innerHTML = `
      <style>
        .mask { position:fixed; inset:0; z-index:2147483647; display:flex; align-items:center; justify-content:center;
                background:rgba(0,0,0,.35); font:13px/1.6 "Segoe UI","Microsoft YaHei",sans-serif;
                color: light-dark(#1f2328,#e8eaed); }
        .card { width:660px; max-width:calc(100vw - 48px); max-height:82vh; display:flex; flex-direction:column;
                border-radius:12px; padding:18px 20px; background: light-dark(#fff,#2a2a2d);
                box-shadow:0 8px 32px rgba(0,0,0,.35); animation: dshPopIn .18s ease-out; }
        .body { overflow:auto; flex:1; min-height:0; }
        .t { font-size:15px; font-weight:600; margin-bottom:6px; }
        .meta { font-size:12px; opacity:.7; margin-bottom:6px; }
        .sec h3 { font-size:12px; opacity:.6; font-weight:600; margin:12px 0 6px;
                  text-transform:uppercase; letter-spacing:.04em; }
        .row { display:flex; align-items:center; gap:8px; padding:7px 10px; border:1px solid light-dark(#e4e7ec,#3a3d44);
               border-radius:8px; margin-bottom:6px; }
        .row .nm { font-weight:600; }
        .row .ver { font-size:11px; opacity:.6; white-space:nowrap; }
        .row .desc { flex:1; font-size:12px; opacity:.8; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
        .row .by { font-size:11px; opacity:.5; white-space:nowrap; }
        .warn { font-size:12px; padding:7px 10px; border-radius:8px; margin-bottom:8px;
                border:1px solid #d9a13b; background:light-dark(#fdf6e7,#3d3524); color:light-dark(#7a5600,#f0c674); }
        .out { font-family:Consolas,monospace; font-size:11px; line-height:1.5; white-space:pre-wrap; word-break:break-all;
               max-height:150px; overflow:auto; background:light-dark(#f6f8fa,#1f2124);
               border:1px solid light-dark(#dde1e7,#3a3d44); border-radius:8px; padding:8px 10px; margin:8px 0; }
        input.inp { flex:1; padding:6px 10px; border-radius:7px; border:1px solid light-dark(#dde1e7,#44464c);
                    background:light-dark(#f7f8fa,#1f2124); color:inherit; font-size:13px; outline:none; }
        input.inp:focus { border-color:#4d7dfe; }
        .btns { display:flex; justify-content:flex-end; gap:8px; margin-top:12px; }
        button.b { padding:6px 14px; border-radius:7px; border:1px solid light-dark(#dde1e7,#44464c);
                   background: light-dark(#f7f8fa,#3a3d44); cursor:pointer; font-size:13px;
                   color: inherit; transition: background .12s ease, transform .08s ease; }
        button.b:hover { background: light-dark(#eef1f6,#484b54); }
        button.b:active { transform: scale(.97); }
        button.b.primary { background:#4d7dfe; border-color:#4d7dfe; color:#fff; }
        button.b.primary:hover { background:#3f6ce8; }
        button.b.danger { border-color:#e81123; color:#e81123; }
        button.b.small { padding:3px 10px; font-size:12px; }
      </style>
      <div class="mask"><div class="card"><div class="body">${html}</div><div class="btns">${btnHtml}</div></div></div>`;
    for (const b of buttons) {
      shadow.querySelector(`[data-b="${b.id}"]`).onclick = () => b.act(shadow);
    }
    return shadow;
  }
  function closeMarket() { marketHost?.remove(); marketHost = null; }

  async function pluginMarket() {
    showMarket(`<div class="t">插件市场</div><div class="meta">加载中…</div>`,
      [{ id: 'ok', label: '关闭', act: () => closeMarket() }]);
    const [lst, cat] = await Promise.all([
      api('/plugin/list').then(r => r.json()).catch(() => null),
      api('/plugin/catalog').then(r => r.json()).catch(() => null),
    ]);
    renderMarket(lst, cat);
  }

  function renderMarket(lst, cat) {
    const shadow = showMarket(marketBody(lst, cat), [
      { id: 'refresh', label: '刷新', act: () => pluginMarket() },
      { id: 'ok', label: '关闭', act: () => closeMarket() },
    ]);
    const pnpmBtn = shadow.querySelector('#market-pnpm');
    if (pnpmBtn) pnpmBtn.onclick = () => ensurePnpm();
    const search = shadow.querySelector('#market-search');
    if (search) {
      search.oninput = () => {
        const q = search.value.trim().toLowerCase();
        shadow.querySelectorAll('.catrow').forEach(row => {
          row.style.display = (row.dataset.q || '').includes(q) ? '' : 'none';
        });
      };
    }
    const addBtn = shadow.querySelector('#market-add-btn');
    const addInp = shadow.querySelector('#market-add-inp');
    if (addBtn && addInp) {
      addBtn.onclick = () => {
        const name = addInp.value.trim();
        if (name) confirmPlugin(name, null);
      };
      addInp.onkeydown = e => { if (e.key === 'Enter' && addInp.value.trim()) confirmPlugin(addInp.value.trim(), null); };
    }
    shadow.querySelectorAll('[data-install]').forEach(b => {
      b.onclick = () => confirmPlugin(b.dataset.install, b.dataset.trusted === '1' ? b.dataset.entry : null);
    });
    shadow.querySelectorAll('[data-remove]').forEach(b => {
      b.onclick = () => confirmRemove(b.dataset.remove);
    });
  }

  function marketBody(lst, cat) {
    const pnpm = !!(lst && lst.ok && lst.pnpm);
    const installed = (lst && lst.ok && lst.plugins) ? lst.plugins : [];
    const installedNames = new Set(installed.map(p => p.name));
    const entries = (cat && cat.ok && cat.plugins) ? cat.plugins : [];
    let html = `<div class="t">插件市场</div>`;
    if (lst && !lst.ok) html += `<div class="warn">已安装列表加载失败：${esc(lst.error || '')}</div>`;
    if (!pnpm) {
      html += `<div class="warn">未检测到 pnpm —— dsh 的插件管理依赖它（pnpm add/remove 转发）。
               <button class="b small" id="market-pnpm" style="margin-left:6px">一键安装 pnpm</button></div>`;
    }
    html += `<div class="sec"><h3>已安装（${installed.length}）</h3>`;
    if (installed.length === 0) html += `<div class="meta">尚未安装插件</div>`;
    for (const p of installed) {
      html += `<div class="row"><span class="nm">${esc(p.name)}</span><span class="ver">${esc(p.version)}</span>
        <span class="desc"></span><button class="b small danger" data-remove="${esc(p.name)}">卸载</button></div>`;
    }
    html += `</div>`;
    html += `<div class="sec"><h3>官方目录（${entries.length}）</h3>`;
    if (cat && !cat.ok) html += `<div class="warn">目录加载失败：${esc(cat.error || '')}</div>`;
    if (entries.length === 0) {
      html += `<div class="meta">目录暂无收录。可用下方「按包名安装」，或向目录仓库提交收录（catalog.json）。</div>`;
    } else {
      html += `<input class="inp" id="market-search" placeholder="搜索目录…" style="margin-bottom:8px">`;
      for (const e of entries) {
        const isInstalled = installedNames.has(e.name);
        html += `<div class="row catrow" data-q="${esc((e.name + ' ' + e.title + ' ' + e.author).toLowerCase())}">
          <span class="nm">${esc(e.title || e.name)}</span>
          <span class="ver">${e.version ? 'v' + esc(e.version) : ''}${e.dsh ? ' · dsh ' + esc(e.dsh) : ''}</span>
          <span class="desc">${esc(e.description || '')}</span>
          <span class="by">${esc(e.author || '')}</span>
          ${isInstalled ? '<span class="ver">已安装</span>'
            : `<button class="b small primary" data-install="${esc(e.name)}" data-trusted="1"
                 data-entry="${esc(JSON.stringify(e))}">安装</button>`}
        </div>`;
      }
    }
    html += `</div>`;
    html += `<div class="sec"><h3>按包名安装</h3>
      <div style="display:flex; gap:8px; align-items:center">
        <input class="inp" id="market-add-inp" placeholder="npm 包名，如 @scope/plugin-name">
        <button class="b" id="market-add-btn">安装</button>
      </div>
      <div class="meta" style="margin-top:6px">未收录的包来源未经验证，安装前会再次确认；仅接受 npm 注册表包名。</div>
    </div>`;
    return html;
  }

  function confirmPlugin(name, entry) {
    let info = `<div class="meta">安装来源：npm 注册表（pnpm add ${esc(name)}），安装后需重启应用生效。</div>`;
    if (entry) {
      info = `<div class="meta">${esc(entry.title || entry.name)}${entry.author ? ' · ' + esc(entry.author) : ''}${entry.version ? ' · v' + esc(entry.version) : ''}${entry.repo ? ' · ' + esc(entry.repo) : ''}</div>` +
             (entry.description ? `<div class="meta">${esc(entry.description)}</div>` : '') +
             `<div class="meta">已收录插件：安装前可对照目录信息确认来源。安装后需重启应用生效。</div>`;
    } else {
      info = `<div class="warn">${esc(name)} 不在官方目录中，来源未经验证。插件安装时会执行其安装脚本（等同运行任意代码），请确认来自可信来源。</div>` + info;
    }
    showMarket(`<div class="t">安装插件 ${esc(name)}</div>${info}`,
      [{ id: 'go', label: '确认安装', primary: true, act: () => startPluginJob('install', name) },
       { id: 'back', label: '返回', act: () => pluginMarket() },
       { id: 'ok', label: '关闭', act: () => closeMarket() }]);
  }

  function confirmRemove(name) {
    showMarket(`<div class="t">卸载插件 ${esc(name)}</div><div class="meta">卸载后需重启应用生效。</div>`,
      [{ id: 'go', label: '确认卸载', primary: true, act: () => startPluginJob('remove', name) },
       { id: 'back', label: '返回', act: () => pluginMarket() },
       { id: 'ok', label: '关闭', act: () => closeMarket() }]);
  }

  function jobLabel(kind) {
    return kind === 'install' ? '安装' : kind === 'remove' ? '卸载' : '安装 pnpm';
  }

  async function ensurePnpm() {
    showMarket(`<div class="t">正在安装 pnpm…</div><div class="meta">执行 npm install -g pnpm（全局安装，需要网络）</div><div class="out"></div>`,
      [{ id: 'ok', label: '后台继续', act: () => closeMarket() }]);
    try { await (await api('/plugin/ensure-pnpm')).json(); } catch (e) { /* poll surfaces */ }
    await pollPluginJob();
  }

  async function startPluginJob(kind, name) {
    showMarket(`<div class="t">${jobLabel(kind)}中… ${esc(name)}</div><div class="out"></div>`,
      [{ id: 'ok', label: '后台继续', act: () => closeMarket() }]);
    const ep = kind === 'install' ? '/plugin/install' : '/plugin/remove';
    try { await (await api(ep + '?pkg=' + encodeURIComponent(name))).json(); } catch (e) { /* poll surfaces */ }
    await pollPluginJob();
  }

  function pollPluginJob() {
    return new Promise(resolve => {
      const poll = setInterval(async () => {
        let r;
        try { r = await (await api('/plugin/status')).json(); } catch (e) { return; }
        if (!r || !r.ok) return;
        const job = r.job;
        if (!job) { clearInterval(poll); resolve(); return; }
        const out = job.output ? `<div class="out">${esc(job.output)}</div>` : '';
        if (job.state === 'running') {
          showMarket(`<div class="t">${jobLabel(job.kind)}中… ${esc(job.pkg || '')}</div>${out}`,
            [{ id: 'ok', label: '后台继续', act: () => closeMarket() }]);
        } else if (job.state === 'done') {
          clearInterval(poll);
          const needRestart = job.kind === 'install' || job.kind === 'remove';
          showMarket(`<div class="t">${jobLabel(job.kind)}完成</div>` +
            `<div class="meta">${esc(job.pkg || '')}${needRestart ? ' · 重启应用后生效' : ''}</div>${out}`,
            needRestart
              ? [{ id: 'go', label: '立即重启', primary: true, act: () => cmd('/win/restart') },
                 { id: 'back', label: '返回市场', act: () => pluginMarket() },
                 { id: 'ok', label: '关闭', act: () => closeMarket() }]
              : [{ id: 'back', label: '返回市场', act: () => pluginMarket() },
                 { id: 'ok', label: '关闭', act: () => closeMarket() }]);
          resolve();
        } else {
          clearInterval(poll);
          const pnpmMissing = /pnpm/.test(job.message || '');
          const hint = pnpmMissing
            ? `<div class="warn">${esc(job.message)}</div>`
            : `<div class="meta">${esc(job.message || '未知错误')}</div>`;
          showMarket(`<div class="t">操作失败</div>${hint}${out}`,
            (pnpmMissing ? [{ id: 'pnpm', label: '一键安装 pnpm', act: () => ensurePnpm() }] : [])
              .concat([{ id: 'back', label: '返回市场', act: () => pluginMarket() },
                       { id: 'ok', label: '关闭', act: () => closeMarket() }]));
          resolve();
        }
      }, 600);
    });
  }

  // ---------------- 语音文本注入（工具栏按钮在 composer toolbar 里） ----------------
  // /speech/stop 返回的文本注入 dsh 的受控输入框
  // （native setter + input 事件绕过 React 检查）。
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

  function init() {
    buildTitlebar();
    installChromeStyle();
    if (onDsh) { buildComposerToolbar(); applyState(); installSettingsPageStyle(); }
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})()
