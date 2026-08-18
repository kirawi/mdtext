import init, {
    StreamingRenderer,
    gfmOptions,
    mathOptions,
    render as renderMarkdown
} from './pkg/mdtext.js';

const editor = document.querySelector('#editor');
const preview = document.querySelector('#preview');
const replay = document.querySelector('#replay');
const inputSignal = document.querySelector('#input-signal');
const outputSignal = document.querySelector('#output-signal');
const inputStatus = document.querySelector('#input-status');
const outputStatus = document.querySelector('#output-status');
const streamSpeed = document.querySelector('#stream-speed');
const streamSpeedValue = document.querySelector('#stream-speed-value');
const instantMode = document.querySelector('#instant-mode');

let options = 0;
let generation = 0;
let editTimer;
let activeOutput;
let sampleMarkdown = '';
let follow = true;
let paintQueued = false;
let streamComplete = true;
let revealSource = null;
let observedFrameDocument = null;
let frameObserver = null;

const RESUME_THRESHOLD = 96;
const MATH_COMPLETE_MARKER = '<!--mdtext-math-complete-->';
const CODE_COMPLETE_MARKER = '<!--mdtext-code-complete-->';

const workerRegistration = navigator.serviceWorker ?
    registerPreviewWorker() :
    Promise.reject(new Error('Service workers are unavailable'));

function waitForActivation(worker) {
    if (!worker || worker.state === 'activated') return Promise.resolve();
    return new Promise(resolve => {
        const onStateChange = () => {
            if (worker.state !== 'activated' && worker.state !== 'redundant') return;
            worker.removeEventListener('statechange', onStateChange);
            resolve();
        };
        worker.addEventListener('statechange', onStateChange);
    });
}

async function registerPreviewWorker() {
    const registration = await navigator.serviceWorker.register('./preview-worker.js?v=katex-3', {
        scope: './',
        updateViaCache: 'none',
    });
    await waitForActivation(registration.installing || registration.waiting);
    await navigator.serviceWorker.ready;
    return registration;
}

function wait(milliseconds) {
    return new Promise(resolve => window.setTimeout(resolve, milliseconds));
}

function speedMultiplier() {
    return 2 ** Number(streamSpeed.value);
}

function streamDelay() {
    return 28 / speedMultiplier();
}

function displaySpeed() {
    const multiplier = speedMultiplier();
    streamSpeedValue.value = `${Number(multiplier.toFixed(2))}×`;
}

function nextFrame() {
    return new Promise(resolve => window.requestAnimationFrame(resolve));
}

function ensureFollowIndicator(doc) {
    if (!doc.body || doc.querySelector('#following')) return;
    const indicator = doc.createElement('button');
    indicator.id = 'following';
    indicator.type = 'button';
    indicator.textContent = '↓ Following paused — scroll to bottom to resume';
    indicator.addEventListener('click', () => {
        follow = true;
        doc.documentElement.scrollTop = doc.documentElement.scrollHeight;
        indicator.remove();
    });
    doc.body.appendChild(indicator);
}

function onFrameScroll(event) {
    const doc = event.currentTarget;
    if (doc !== preview.contentDocument) return;
    const element = doc.documentElement;
    const atBottom = element.scrollHeight - element.scrollTop - element.clientHeight < RESUME_THRESHOLD;
    if (!follow && atBottom) {
        follow = true;
        element.scrollTop = element.scrollHeight;
        doc.querySelector('#following')?.remove();
    }
}

function onFrameWheel(event) {
    if (event.deltaY >= 0 || !follow) return;
    const doc = event.currentTarget;
    if (doc !== preview.contentDocument) return;
    follow = false;
    ensureFollowIndicator(doc);
}

function scheduleFollow() {
    if (paintQueued) return;
    paintQueued = true;
    window.requestAnimationFrame(() => {
        paintQueued = false;
        const doc = preview.contentDocument;
        if (!doc?.body) return;
        renderMath(doc.body);
        highlightCode(doc.body);
        if (follow) {
            doc.documentElement.scrollTop = doc.documentElement.scrollHeight;
        } else {
            ensureFollowIndicator(doc);
        }
    });
}

function renderMath(root) {
    if (!globalThis.katex) return;
    const styles = root.ownerDocument.querySelector('#katex-styles');
    if (!styles?.dataset.loaded) return;
    root.querySelectorAll('.math:not([data-mdtext-rendered])').forEach(element => {
        let marker = element.nextSibling;
        while (marker?.nodeType === Node.TEXT_NODE && !marker.data.trim()) {
            marker = marker.nextSibling;
        }
        const hasCompletionMarker = marker?.nodeType === Node.COMMENT_NODE &&
            marker.data === 'mdtext-math-complete';
        if (!streamComplete && !hasCompletionMarker) return;
        if (hasCompletionMarker) marker.remove();

        const source = element.textContent;
        if (!source) return;
        element.dataset.mdtextRendered = 'true';
        try {
            element.innerHTML = globalThis.katex.renderToString(source, {
                displayMode: element.classList.contains('math-display'),
                throwOnError: false,
            });
        } catch (error) {
            element.classList.add('math-error');
            element.setAttribute('title', error.message);
        }
    });
}

function observeKaTeXStyles(doc) {
    const styles = doc.querySelector('#katex-styles');
    if (!styles || styles.dataset.observed) return;
    styles.dataset.observed = 'true';
    const loaded = () => {
        styles.dataset.loaded = 'true';
        scheduleFollow();
    };
    if (styles.sheet) {
        loaded();
    } else {
        styles.addEventListener('load', loaded, {
            once: true
        });
        styles.addEventListener('error', () => {
            outputStatus.textContent = 'output complete · KaTeX stylesheet unavailable';
        }, {
            once: true
        });
    }
}

function highlightCode(root) {
    if (!globalThis.hljs) return;
    root.querySelectorAll('pre > code[class*="language-"]:not([data-highlighted])').forEach(element => {
        const container = element.parentElement;
        let marker = container.nextSibling;
        while (marker?.nodeType === Node.TEXT_NODE && !marker.data.trim()) {
            marker = marker.nextSibling;
        }
        const hasCompletionMarker = marker?.nodeType === Node.COMMENT_NODE &&
            marker.data === 'mdtext-code-complete';
        if (!streamComplete && !hasCompletionMarker) return;
        if (hasCompletionMarker) marker.remove();
        const languageClass = [...element.classList].find(name => name.startsWith('language-'));
        const language = languageClass?.slice('language-'.length);
        if (!language || !globalThis.hljs.getLanguage(language)) {
            element.dataset.highlighted = 'unsupported';
            return;
        }
        globalThis.hljs.highlightElement(element);
    });
}

function markCompletedOutput(html) {
    return html
        .replace(
            /(<span class="math math-(?:inline|display)">[\s\S]*?<\/span>)/g,
            `$1${MATH_COMPLETE_MARKER}`,
        )
        .replace(
            /(<pre><code class="language-[^"]+">[\s\S]*?<\/code><\/pre>)/g,
            `$1${CODE_COMPLETE_MARKER}`,
        );
}

function observeStreamingFrame(doc) {
    if (!doc || doc === observedFrameDocument || doc.body?.id !== 'mdtext-preview-body') {
        return false;
    }
    frameObserver?.disconnect();
    observedFrameDocument = doc;
    frameObserver = new MutationObserver(scheduleFollow);
    frameObserver.observe(doc, {
        childList: true,
        subtree: true,
        characterData: true
    });
    observeKaTeXStyles(doc);
    doc.addEventListener('scroll', onFrameScroll, {
        passive: true
    });
    doc.addEventListener('wheel', onFrameWheel, {
        passive: true
    });
    scheduleFollow();
    return true;
}

function monitorFrame(current) {
    if (current !== generation || streamComplete) return;
    const doc = preview.contentDocument;
    observeStreamingFrame(doc);
    if (doc === observedFrameDocument) scheduleFollow();
    window.setTimeout(() => monitorFrame(current), 50);
}

preview.addEventListener('load', () => {
    const loadedGeneration = Number(new URL(preview.contentWindow.location.href).searchParams.get('id'));
    if (!loadedGeneration || loadedGeneration !== generation) return;
    const doc = preview.contentDocument;
    observeStreamingFrame(doc);
    streamComplete = true;
    scheduleFollow();
});

async function loadSample() {
    const response = await fetch('./demo.md');
    if (!response.ok) throw new Error(`could not load demo.md (${response.status})`);
    return response.text();
}

function readUpdate(update) {
    const value = {
        html: markCompletedOutput(update.htmlDelta),
        buffered: update.bufferedBytes,
    };
    update.free();
    return value;
}

function formatDuration(milliseconds) {
    if (milliseconds < 0.1) return '<0.1 ms';
    if (milliseconds < 10) return `${milliseconds.toFixed(2)} ms`;
    return `${milliseconds.toFixed(1)} ms`;
}

function pendingInput() {
    if (!revealSource) return editor.value;
    const source = revealSource;
    revealSource = null;
    editor.value = source;
    editor.scrollTop = editor.scrollHeight;
    return source;
}

async function renderInstantly(markdown) {
    const current = ++generation;
    const output = await createOutputStream(current);
    if (current !== generation) return;

    editor.readOnly = false;
    replay.disabled = true;
    inputSignal.classList.add('active');
    outputSignal.classList.add('active');
    inputStatus.textContent = 'reading complete input';
    outputStatus.textContent = 'rendering';

    try {
        const started = performance.now();
        const rendered = renderMarkdown(markdown, options);
        const duration = performance.now() - started;
        const html = markCompletedOutput(rendered);
        if (html) output.postMessage({
            type: 'chunk',
            html
        });
        output.postMessage({
            type: 'close'
        });
        inputSignal.classList.remove('active');
        outputSignal.classList.remove('active');
        inputStatus.textContent = 'input complete';
        outputStatus.textContent = `parsed in ${formatDuration(duration)}`;
        replay.disabled = false;
    } catch (error) {
        streamComplete = true;
        inputSignal.classList.remove('active');
        outputSignal.classList.remove('active');
        inputStatus.textContent = 'input unavailable';
        outputStatus.textContent = `renderer error: ${error.message}`;
        replay.disabled = false;
        throw error;
    }
}

function renderCurrent(markdown, streamingOptions) {
    if (instantMode.checked && !streamingOptions.revealInput) return renderInstantly(markdown);
    return streamDocument(markdown, streamingOptions);
}

async function createOutputStream(id) {
    if (activeOutput) {
        activeOutput.close();
        activeOutput = null;
    }

    const registration = await workerRegistration;
    const channel = new MessageChannel();
    await new Promise((resolve, reject) => {
        const timeout = window.setTimeout(() => reject(new Error('Preview stream did not start')), 3000);
        channel.port1.onmessage = event => {
            if (event.data?.type !== 'ready') return;
            window.clearTimeout(timeout);
            resolve();
        };
        channel.port1.start();
        registration.active.postMessage({
            type: 'create-stream',
            id
        }, [channel.port2]);
    });

    preview.removeAttribute('srcdoc');
    follow = true;
    streamComplete = false;
    frameObserver?.disconnect();
    observedFrameDocument = null;
    preview.src = `./preview-stream?id=${encodeURIComponent(id)}`;
    window.requestAnimationFrame(() => monitorFrame(id));
    activeOutput = channel.port1;
    return channel.port1;
}

async function streamDocument(markdown, {
    revealInput,
    animate
}) {
    const current = ++generation;
    const renderer = new StreamingRenderer(options);
    const output = await createOutputStream(current);
    const chunkSize = animate ? 5 : 2048;
    let offset = 0;

    if (current !== generation) {
        renderer.free();
        return;
    }
    if (revealInput) {
        revealSource = markdown;
        editor.value = '';
    }
    editor.readOnly = revealInput;
    replay.disabled = true;
    inputSignal.classList.add('active');
    outputSignal.classList.remove('active');
    inputStatus.textContent = 'streaming input';
    outputStatus.textContent = 'waiting for output';

    try {
        while (offset < markdown.length) {
            if (current !== generation) return;
            let end = Math.min(offset + chunkSize, markdown.length);
            const finalUnit = markdown.charCodeAt(end - 1);
            const nextUnit = markdown.charCodeAt(end);
            if (end < markdown.length &&
                finalUnit >= 0xD800 && finalUnit <= 0xDBFF &&
                nextUnit >= 0xDC00 && nextUnit <= 0xDFFF) {
                end += 1;
            }
            const chunk = markdown.slice(offset, end);
            offset = end;
            if (revealInput) {
                editor.value += chunk;
                editor.scrollTop = editor.scrollHeight;
            }

            const update = readUpdate(renderer.push(chunk));

            if (update.html) {
                output.postMessage({
                    type: 'chunk',
                    html: update.html
                });
                outputSignal.classList.add('active');
                outputStatus.textContent = 'streaming output';
            } else {
                outputSignal.classList.remove('active');
                outputStatus.textContent = update.buffered ? 'buffering a block' : 'waiting for output';
            }

            if (animate) await wait(streamDelay());
            else await nextFrame();
        }

        if (current !== generation) return;
        const finalUpdate = readUpdate(renderer.finish());
        if (finalUpdate.html) {
            output.postMessage({
                type: 'chunk',
                html: finalUpdate.html
            });
        }
        output.postMessage({
            type: 'close'
        });
        revealSource = null;
        inputSignal.classList.remove('active');
        outputSignal.classList.remove('active');
        inputStatus.textContent = 'input complete';
        outputStatus.textContent = 'output complete';
        editor.readOnly = false;
        replay.disabled = false;
    } catch (error) {
        streamComplete = true;
        revealSource = null;
        inputSignal.classList.remove('active');
        outputSignal.classList.remove('active');
        inputStatus.textContent = 'stream stopped';
        outputStatus.textContent = `renderer error: ${error.message}`;
        editor.readOnly = false;
        replay.disabled = false;
        throw error;
    } finally {
        renderer.free();
    }
}

replay.addEventListener('click', () => {
    renderCurrent(editor.value, {
        revealInput: true,
        animate: true
    });
});

streamSpeed.addEventListener('input', displaySpeed);
displaySpeed();

instantMode.addEventListener('change', () => {
    streamSpeed.disabled = instantMode.checked;
    if (instantMode.checked) renderInstantly(pendingInput());
});

editor.addEventListener('input', event => {
    window.clearTimeout(editTimer);
    editTimer = window.setTimeout(() => {
        const pasted = event.inputType === 'insertFromPaste';
        renderCurrent(editor.value, {
            revealInput: false,
            animate: pasted
        });
    }, 90);
});

try {
    const [, sample] = await Promise.all([init(), loadSample()]);
    sampleMarkdown = sample;
    options = gfmOptions() | mathOptions();
    await streamDocument(sampleMarkdown, {
        revealInput: true,
        animate: true
    });
} catch (error) {
    inputStatus.textContent = 'input unavailable';
    outputStatus.textContent = `could not start: ${error.message}`;
    console.error(error);
}