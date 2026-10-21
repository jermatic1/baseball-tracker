// Shared by the tracker's pages: one socket that says hello, retries on
// close, and dispatches each JSON message to the handler named by its type.
// Binary frames go to `blob`; `open` and `close` are called on connection
// changes.
let ws = null;
let retry = 1000;
let handlers = {};

function connect(client, h) {
  handlers = h;
  ws = new WebSocket(`ws://${location.host}/ws`);
  ws.binaryType = "blob";
  ws.onopen = () => {
    retry = 1000;
    send({ type: "hello", client });
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
    setTimeout(() => connect(client, handlers), retry);
    retry = Math.min(retry * 2, 10000);
  };
}

function send(msg) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function cell(row, text) {
  const td = document.createElement("td");
  td.textContent = text;
  row.appendChild(td);
}
