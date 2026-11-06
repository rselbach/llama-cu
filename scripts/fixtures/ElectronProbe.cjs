// A disposable Chromium app for check-background.py. Never focuses its window.
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(process.resourcesPath, '../../..');
const output = path.join(root, 'web-state.json');
fs.mkdirSync(path.join(root, 'electron-profile'), { recursive: true });
app.setPath('userData', path.join(root, 'electron-profile'));

app.whenReady().then(async () => {
  app.setAccessibilitySupportEnabled(true);
  const win = new BrowserWindow({
    title: 'Greendale Web', width: 500, height: 400, x: 100, y: 100,
    show: false, webPreferences: { backgroundThrottling: false },
  });
  let everFocused = false;
  win.on('focus', () => { everFocused = true; });
  await win.loadURL('data:text/html;charset=utf-8,' + encodeURIComponent(`
    <!doctype html><html><head><title>Greendale Web</title></head>
    <body style="background:#e8f4ff; margin:30px; min-height:1800px">
      <h1>Greendale Community College</h1>
      <button id="enroll" onclick="window.presses++">Enroll</button>
      <label>Name <input id="name" aria-label="Name"></label>
      <div id="canvas" style="margin-top:20px;width:300px;height:100px;background:#ade">
        Drag here
      </div>
      <script>
        window.presses = 0;
        window.moves = 0;
        window.heldMoves = 0;
        window.downs = 0;
        window.dragging = false;
        const canvas = document.getElementById('canvas');
        canvas.addEventListener('mousedown', () => {
          window.downs++;
          window.dragging = true;
        });
        canvas.addEventListener('mouseup', () => { window.dragging = false; });
        canvas.addEventListener('mousemove', e => {
          if (!window.dragging) return;
          window.moves++;
          if (e.buttons) window.heldMoves++;
        });
        document.getElementById('name').focus();
      </script>
    </body></html>`));
  win.showInactive();
  async function report() {
    if (win.isDestroyed()) return;
    try {
      const state = await win.webContents.executeJavaScript(`({
        presses: window.presses, text: document.getElementById('name').value,
        downs: window.downs, moves: window.moves, heldMoves: window.heldMoves,
        scroll: window.scrollY,
        canvas: document.getElementById('canvas').getBoundingClientRect().toJSON()
      })`);
      const content = win.getContentBounds();
      const bounds = win.getBounds();
      state.canvas.x += content.x - bounds.x;
      state.canvas.y += content.y - bounds.y;
      Object.assign(state, { pid: process.pid, everFocused, versions: process.versions });
      fs.writeFileSync(output + '.tmp', JSON.stringify(state));
      fs.renameSync(output + '.tmp', output);
      setTimeout(report, 20);
    } catch (error) {
      console.error(error);
      app.exit(1);
    }
  }
  await report();
}).catch(error => {
  console.error(error);
  app.exit(1);
});
app.on('window-all-closed', () => app.quit());
