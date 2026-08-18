const encoder = new TextEncoder();
const streams = new Map();

const DOCUMENT_START = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link id="katex-styles" rel="stylesheet" href="https://cdn.jsdelivr.net/npm/katex@0.16.22/dist/katex.min.css" integrity="sha384-5TcZemv2l/9On385z///+d7MSYlvIEw9FuZTIdZ14vJLqWphw7e7ZPuOiCHJcFCP" crossorigin="anonymous"><link rel="stylesheet" href="./preview.css"></head><body id="mdtext-preview-body">`;
const DOCUMENT_END = '</body></html>';

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));

function closeStream(id, state) {
    if (state.controller) {
        state.controller.enqueue(encoder.encode(DOCUMENT_END));
        state.controller.close();
    } else {
        state.closed = true;
    }
    if (state.controller) {
        state.port.close();
        streams.delete(id);
    }
}

self.addEventListener('message', event => {
    if (event.data?.type !== 'create-stream' || !event.ports[0]) return;

    const id = String(event.data.id);
    const state = {
        port: event.ports[0],
        controller: null,
        chunks: [],
        closed: false,
    };

    state.port.onmessage = message => {
        if (message.data?.type === 'chunk') {
            const bytes = encoder.encode(message.data.html);
            if (state.controller) state.controller.enqueue(bytes);
            else state.chunks.push(bytes);
        } else if (message.data?.type === 'close') {
            closeStream(id, state);
        }
    };
    state.port.start();
    streams.set(id, state);
    state.port.postMessage({
        type: 'ready'
    });
});

self.addEventListener('fetch', event => {
    const url = new URL(event.request.url);
    if (!url.pathname.endsWith('/preview-stream')) return;

    const id = url.searchParams.get('id');
    const state = streams.get(id);
    if (!state) {
        event.respondWith(new Response('Preview stream not found', {
            status: 404
        }));
        return;
    }

    const body = new ReadableStream({
        start(controller) {
            state.controller = controller;
            controller.enqueue(encoder.encode(DOCUMENT_START));
            for (const chunk of state.chunks) controller.enqueue(chunk);
            state.chunks.length = 0;
            if (state.closed) closeStream(id, state);
        },
        cancel() {
            state.port.close();
            streams.delete(id);
        },
    });

    event.respondWith(new Response(body, {
        headers: {
            'Content-Type': 'text/html; charset=utf-8',
            'Cache-Control': 'no-store',
            'Content-Security-Policy': "default-src 'none'; style-src 'self' 'unsafe-inline' https://cdn.jsdelivr.net; font-src https://cdn.jsdelivr.net; img-src https: data:; script-src 'none'; object-src 'none'; form-action 'none'",
            'X-Content-Type-Options': 'nosniff',
        },
    }));
});