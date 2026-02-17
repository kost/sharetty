import { Terminal } from 'xterm';
import { FitAddon } from 'xterm-addon-fit';
import { WebLinksAddon } from 'xterm-addon-web-links';
import 'xterm/css/xterm.css';
import './style.css'; // Import custom styles
import { Overlay } from './overlay'; // Import Overlay

// Initialize Overlay
const overlay = new Overlay();

// Initialize Terminal
const term = new Terminal({
    cursorBlink: true,
    macOptionIsMeta: true,
    scrollback: 1000,
});

// Load Addons
const fitAddon = new FitAddon();
term.loadAddon(fitAddon);
term.loadAddon(new WebLinksAddon());

// Open Terminal
const terminalElement = document.getElementById('terminal');
if (terminalElement) {
    term.open(terminalElement);
    fitAddon.fit();
}

// Handle Resize
window.addEventListener('resize', () => {
    fitAddon.fit();
    sendResize();
});

// WebSocket Connection
let wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
let wsPath = window.location.pathname;
if (!wsPath.endsWith('/')) wsPath += '/';
const ws = new WebSocket(wsProtocol + '//' + window.location.host + wsPath + 'ws');

ws.binaryType = 'arraybuffer';

ws.onopen = () => {
    console.log('WebSocket connected');
    overlay.updateStatus(true);
    sendResize();
};

ws.onmessage = (event) => {
    if (event.data instanceof ArrayBuffer) {
        term.write(new Uint8Array(event.data));
    } else {
        // Handle control messages if backend sends text
        // Currently sharetty sends binary for PTY output
    }
};

ws.onclose = () => {
    console.log('WebSocket closed');
    term.write('\r\nConnection closed.\r\n');
    overlay.updateStatus(false);
};

ws.onerror = (error) => {
    console.error('WebSocket error:', error);
};

// Terminal Input -> WebSocket
term.onData((data) => {
    ws.send(JSON.stringify({ type: 'input', data: data }));
});

function sendResize() {
    const dims = { cols: term.cols, rows: term.rows };
    if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'resize', ...dims }));
    }
}
