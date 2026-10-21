// Shared by the tracker's pages: one socket that says hello, retries on
// close, and dispatches each JSON message to the handler named by its type.
// Binary frames go to `blob`; `open` and `close` are called on connection
// changes. Game bodies arrive as `game` with the game's name.
let ws = null;
let retry = 1000;
let handlers = {};

function connect(client, h, games = []) {
  handlers = h;
  ws = new WebSocket(`ws://${location.host}/ws`);
  ws.binaryType = "blob";
  ws.onopen = () => {
    retry = 1000;
    send({ type: "hello", client, games });
    if (handlers.open) handlers.open();
  };
  ws.onmessage = (e) => {
    if (e.data instanceof Blob) {
      if (handlers.blob) handlers.blob(e.data);
      return;
    }
    const m = JSON.parse(e.data);
    if (handlers[m.type]) handlers[m.type](m);
  };
  ws.onclose = () => {
    if (handlers.close) handlers.close();
    setTimeout(() => connect(client, handlers, games), retry);
    retry = Math.min(retry * 2, 10000);
  };
}

// Add a link for each registered game module after the fixed links.
fetch("/games").then((r) => r.json()).then((names) => {
  const nav = document.querySelector("nav");
  names.forEach((name) => {
    const a = document.createElement("a");
    a.href = `/games/${name}`;
    a.textContent = name[0].toUpperCase() + name.slice(1);
    if (location.pathname === a.getAttribute("href")) a.className = "here";
    nav.appendChild(a);
  });
}).catch(() => {});

function send(msg) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function cell(row, text) {
  const td = document.createElement("td");
  td.textContent = text;
  row.appendChild(td);
}
