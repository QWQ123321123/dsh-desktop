// Generates dist/splash.html: the startup page shown while dsh boots.
// Embeds the DeepSeek whale (assets/deepseek-logo.svg, recolored brand blue)
// with a breathing animation and an indeterminate progress bar.

const fs = require('node:fs');
const path = require('node:path');

const svg = fs
  .readFileSync(path.join(__dirname, '..', 'assets', 'deepseek-logo.svg'), 'utf8')
  .replace(/(<path\b[^>]*?)\s*fill="[^"]*"/, '$1')
  .replace('<path ', '<path fill="#4d6bfe" ')
  .replace(/<style>[\s\S]*?<\/style>/, '')
  .replace(/width="50\.000000" height="50\.000000"/, 'width="96" height="96"');

const html = `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<title>DeepSeek Harness</title>
<style>
  html, body { margin: 0; height: 100%; background: #ffffff; }
  body {
    display: flex; flex-direction: column; align-items: center; justify-content: center;
    gap: 24px; font-family: "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
    user-select: none; cursor: default;
  }
  .logo { animation: breathe 2.2s ease-in-out infinite; }
  @keyframes breathe {
    0%, 100% { transform: scale(1); opacity: 1; }
    50% { transform: scale(1.08); opacity: 0.82; }
  }
  .title { font-size: 15px; font-weight: 600; color: #1f2328; letter-spacing: 0.02em; }
  .bar {
    width: 180px; height: 3px; border-radius: 2px; background: #e8ecf3;
    overflow: hidden; position: relative;
  }
  .bar::after {
    content: ""; position: absolute; top: 0; left: -40%; width: 40%; height: 100%;
    border-radius: 2px; background: #4d6bfe;
    animation: slide 1.2s ease-in-out infinite;
  }
  @keyframes slide {
    0% { left: -40%; } 100% { left: 100%; }
  }
  .status { font-size: 12px; color: #8a919c; }
</style>
</head>
<body>
  <div class="logo">${svg}</div>
  <div class="title">DeepSeek Harness</div>
  <div class="bar"></div>
  <div class="status" id="status">正在启动…</div>
</body>
</html>
`;

fs.mkdirSync(path.join(__dirname, '..', 'dist'), { recursive: true });
fs.writeFileSync(path.join(__dirname, '..', 'dist', 'splash.html'), html);
console.log('splash.html written');
