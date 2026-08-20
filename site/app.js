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
let activeOutputId = null;
let sampleMarkdown = '';
let follow = true;
let paintQueued = false;
let streamComplete = true;
let revealSource = null;
let observedFrameDocument = null;
let frameObserver = null;

const RESUME_THRESHOLD = 96;
const DELIVERY_TIMEOUT = 5000;
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
    const registration = await navigator.serviceWorker.register('./preview-worker.js?v=streams-5', {
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

function createCompletionTracker() {
    return {
        codeDepth: 0,
        mathDepth: 0,
    };
}

function markCompletedOutput(html, tracker) {
    // Opening tags, content, and closing tags commonly arrive in separate
    // StreamingRenderer deltas. Track them across updates so the preview can
    // keep streaming while expensive enhancement waits for a complete block.
    return html.replace(
        /<pre><code(?: class="[^"]*")?>|<\/code><\/pre>|<span class="math math-(?:inline|display)">|<\/span>/g,
        token => {
            if (token.startsWith('<pre><code')) {
                tracker.codeDepth += 1;
                return token;
            }
            if (token === '</code></pre>') {
                if (tracker.codeDepth === 0) return token;
                tracker.codeDepth -= 1;
                return `${token}${CODE_COMPLETE_MARKER}`;
            }
            if (token.startsWith('<span class="math ')) {
                tracker.mathDepth += 1;
                return token;
            }
            if (tracker.mathDepth === 0) return token;
            tracker.mathDepth -= 1;
            return `${token}${MATH_COMPLETE_MARKER}`;
        },
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

function readUpdate(update, completionTracker) {
    const value = {
        html: markCompletedOutput(update.htmlDelta, completionTracker),
        buffered: update.bufferedBytes,
    };
    update.free();
    return value;
}

function failOutputStream(output, error) {
    if (output.failure) return;
    output.failure = error;
    for (const pending of output.pending.values()) {
        window.clearTimeout(pending.timeout);
        pending.reject(error);
    }
    output.pending.clear();
}

function sendOutputMessage(output, message) {
    if (output.failure) return Promise.reject(output.failure);
    const sequence = output.nextSequence++;
    return new Promise((resolve, reject) => {
        const timeout = window.setTimeout(() => {
            output.pending.delete(sequence);
            const error = new Error('preview connection lost (delivery was not acknowledged)');
            failOutputStream(output, error);
            reject(error);
        }, DELIVERY_TIMEOUT);
        output.pending.set(sequence, {
            resolve,
            reject,
            timeout,
        });
        try {
            output.port.postMessage({
                ...message,
                sequence,
            });
        } catch (error) {
            window.clearTimeout(timeout);
            output.pending.delete(sequence);
            failOutputStream(output, error);
            reject(error);
        }
    });
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
    if (current !== generation) {
        closeOutputStream(output, current, 'cancel');
        return;
    }

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
        const html = markCompletedOutput(rendered, createCompletionTracker());
        if (html) await sendOutputMessage(output, {
            type: 'chunk',
            html
        });
        await closeOutputStream(output, current, 'close');
        inputSignal.classList.remove('active');
        outputSignal.classList.remove('active');
        inputStatus.textContent = 'input complete';
        outputStatus.textContent = `parsed in ${formatDuration(duration)}`;
        replay.disabled = false;
    } catch (error) {
        closeOutputStream(output, current, 'cancel');
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
        // Closing a MessagePort does not notify the service worker. Explicitly
        // cancel the old stream so it cannot remain in the worker's stream map
        // after this render supersedes it.
        try {
            activeOutput.port.postMessage({
                type: 'cancel'
            });
            failOutputStream(activeOutput, new DOMException('Render superseded', 'AbortError'));
        } catch {
            // The worker may already have closed the port after a completed
            // stream; there is nothing left to clean up in that case.
        }
        activeOutput = null;
        activeOutputId = null;
    }

    const registration = await workerRegistration;
    const channel = new MessageChannel();
    const output = {
        port: channel.port1,
        nextSequence: 1,
        pending: new Map(),
        failure: null,
    };
    await new Promise((resolve, reject) => {
        const timeout = window.setTimeout(() => {
            try {
                output.port.postMessage({
                    type: 'cancel'
                });
            } catch {
                // There may be no worker endpoint if startup failed.
            }
            reject(new Error('Preview stream did not start'));
        }, 3000);
        output.port.onmessage = event => {
            if (event.data?.type === 'ready') {
                window.clearTimeout(timeout);
                resolve();
                return;
            }
            if (event.data?.type === 'ack') {
                const pending = output.pending.get(event.data.sequence);
                if (!pending) return;
                window.clearTimeout(pending.timeout);
                output.pending.delete(event.data.sequence);
                pending.resolve();
                return;
            }
            if (event.data?.type === 'error') {
                failOutputStream(output, new Error(`preview connection lost: ${event.data.message}`));
            }
        };
        output.port.onmessageerror = () => {
            failOutputStream(output, new Error('preview connection lost (invalid worker message)'));
        };
        output.port.start();
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
    activeOutput = output;
    activeOutputId = id;
    return output;
}

function closeOutputStream(output, id, messageType) {
    if (!output) return;
    let delivery = Promise.resolve();
    try {
        if (messageType === 'close') {
            delivery = sendOutputMessage(output, {
                type: messageType
            });
        } else {
            output.port.postMessage({
                type: messageType
            });
            failOutputStream(output, new DOMException('Render cancelled', 'AbortError'));
        }
    } catch {
        // The worker can close its side immediately after receiving `close`.
    }
    // Leave the port open long enough for the worker to receive the control
    // message. The worker closes its side after handling it.
    if (activeOutput === output && activeOutputId === id) {
        activeOutput = null;
        activeOutputId = null;
    }
    return delivery;
}

async function streamDocument(markdown, {
    revealInput,
    animate
}) {
    const current = ++generation;
    const renderer = new StreamingRenderer(options);
    const completionTracker = createCompletionTracker();
    const output = await createOutputStream(current);
    const chunkSize = animate ? 5 : 2048;
    let offset = 0;

    if (current !== generation) {
        closeOutputStream(output, current, 'cancel');
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
            if (current !== generation) {
                closeOutputStream(output, current, 'cancel');
                return;
            }
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

            const update = readUpdate(renderer.push(chunk), completionTracker);

            if (update.html) {
                await sendOutputMessage(output, {
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

        if (current !== generation) {
            closeOutputStream(output, current, 'cancel');
            return;
        }
        const finalUpdate = readUpdate(renderer.finish(), completionTracker);
        if (finalUpdate.html) {
            await sendOutputMessage(output, {
                type: 'chunk',
                html: finalUpdate.html
            });
        }
        await closeOutputStream(output, current, 'close');
        revealSource = null;
        inputSignal.classList.remove('active');
        outputSignal.classList.remove('active');
        inputStatus.textContent = 'input complete';
        outputStatus.textContent = 'output complete';
        editor.readOnly = false;
        replay.disabled = false;
    } catch (error) {
        closeOutputStream(output, current, 'cancel');
        if (current !== generation) return;
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
