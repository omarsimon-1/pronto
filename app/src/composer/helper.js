(() => {
  'use strict';
  if (window.sealwaxComposer) {
    return;
  }
  const handlers = window.webkit && window.webkit.messageHandlers;
  // Every key the page has seen (counted below): every message says it, so the card knows how
  // far the page is behind the keys it gave the view (Send waits for them).
  let keysSeen = 0;
  const post = (message) => {
    if (handlers && handlers.composer) {
      handlers.composer.postMessage({ ...message, keys: keysSeen });
    }
  };

  const ALPHABET = 'abcdefghijklmnopqrstuvwxyz234567';
  const token = () => {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    let bits = 0;
    let pending = 0;
    let out = '';
    for (const byte of bytes) {
      bits = (bits << 8) | byte;
      pending += 8;
      while (pending >= 5) {
        pending -= 5;
        out += ALPHABET[(bits >> pending) & 31];
      }
      bits &= (1 << pending) - 1;
    }
    if (pending > 0) {
      out += ALPHABET[(bits << (5 - pending)) & 31];
    }
    return out;
  };

  const DROP = new Set([
    'SCRIPT', 'STYLE', 'IFRAME', 'FRAME', 'FRAMESET', 'OBJECT', 'EMBED', 'APPLET', 'FORM',
    'INPUT', 'BUTTON', 'SELECT', 'TEXTAREA', 'OPTION', 'OPTGROUP', 'LABEL', 'FIELDSET',
    'LEGEND', 'DATALIST', 'OUTPUT', 'METER', 'PROGRESS', 'LINK', 'META', 'BASE', 'HEAD',
    'TITLE', 'SVG', 'MATH', 'TEMPLATE', 'NOSCRIPT', 'NOEMBED', 'NOFRAMES', 'AUDIO', 'VIDEO',
    'SOURCE', 'TRACK', 'CANVAS', 'PICTURE', 'MAP', 'AREA', 'DIALOG', 'SLOT', 'PORTAL', 'PARAM',
    'XMP', 'PLAINTEXT',
  ]);
  const KEEP = new Set([
    'P', 'DIV', 'BR', 'B', 'STRONG', 'I', 'EM', 'U', 'S', 'STRIKE', 'DEL', 'SUB', 'SUP', 'A',
    'UL', 'OL', 'LI', 'BLOCKQUOTE', 'PRE', 'CODE', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'HR',
    'TABLE', 'THEAD', 'TBODY', 'TFOOT', 'TR', 'TD', 'TH', 'CAPTION', 'IMG',
  ]);
  const BLOCKS = new Set([
    'SECTION', 'ARTICLE', 'HEADER', 'FOOTER', 'NAV', 'MAIN', 'ASIDE', 'FIGURE', 'FIGCAPTION',
    'ADDRESS', 'DETAILS', 'SUMMARY', 'CENTER', 'DL', 'DT', 'DD', 'HGROUP',
  ]);
  const VOID = new Set(['BR', 'HR', 'IMG']);
  const SEMANTIC = { B: 'B', STRONG: 'B', I: 'I', EM: 'I', U: 'U', S: 'S', STRIKE: 'S', DEL: 'S' };
  const IMAGE_SOURCE = /^data:image\/(png|jpeg|gif|webp);base64,[a-z0-9+/=\s]*$/i;
  const CID_SOURCE = /^cid:[^\s"'<>\\]{1,250}$/i;
  const LINK = /^(https?:\/\/|mailto:)/i;
  const REMOTE = /^\s*(https?:)?\/\//i;
  const URL_ATTRIBUTE = /^(src|href|background|poster|srcset|action|formaction|data|codebase|cite|longdesc|lowsrc|dynsrc|ping|xlink:href)$/;
  const INDENT_STYLE = 'margin: 0 0 0 40px; border: none; padding: 0px;';
  const QUOTE_STYLE = 'margin:0 0 0 0.8ex;border-left:2px solid #8a8a8a;padding-left:1ex';
  const MAX_IMAGE = 10 * 1024 * 1024;
  const TOKEN = /^[a-z2-7]{26}$/;

  const emptyStats = () => ({
    dropped: {}, handlers: 0, styles: 0, attributes: 0, remote: 0, images: 0, links: 0,
    comments: 0, unwrapped: 0, deep: 0,
  });

  const emphasis = (element) => {
    if (!element.getAttribute('style')) {
      return [];
    }
    const style = element.style;
    const wrappers = [];
    const weight = style.fontWeight;
    if (weight === 'bold' || weight === 'bolder' || Number(weight) >= 600) {
      wrappers.push('B');
    }
    if (style.fontStyle === 'italic' || style.fontStyle === 'oblique') {
      wrappers.push('I');
    }
    const lines = `${style.textDecorationLine || ''} ${style.textDecoration || ''}`;
    if (lines.includes('underline')) {
      wrappers.push('U');
    }
    if (lines.includes('line-through')) {
      wrappers.push('S');
    }
    return wrappers;
  };

  const normalWeight = (element) => {
    const weight = element.style && element.style.fontWeight;
    return weight === 'normal' || weight === '400' || weight === 'lighter';
  };

  const countAttributes = (element, stats) => {
    for (const attribute of Array.from(element.attributes)) {
      const name = attribute.name.toLowerCase();
      if (name.startsWith('on')) {
        stats.handlers += 1;
      } else if (name === 'style') {
        stats.styles += 1;
      } else {
        if (URL_ATTRIBUTE.test(name) && REMOTE.test(attribute.value)) {
          stats.remote += 1;
        }
        stats.attributes += 1;
      }
    }
  };

  // `own`: the editor's own document (serialising), whose cid: images are the draft's parts;
  // a paste or a drop never brings cid: references in.
  const copyElement = (source, tag, doc, stats, own) => {
    const copy = doc.createElement(tag === 'STRONG' ? 'B' : tag === 'EM' ? 'I' : tag);
    for (const attribute of Array.from(source.attributes)) {
      const name = attribute.name.toLowerCase();
      const value = attribute.value.trim();
      if (name.startsWith('on')) {
        stats.handlers += 1;
      } else if (tag === 'A' && name === 'href') {
        if (LINK.test(value)) {
          copy.setAttribute('href', value);
        } else {
          stats.links += 1;
        }
      } else if (tag === 'IMG' && name === 'src') {
        if (IMAGE_SOURCE.test(value) || (own && CID_SOURCE.test(value))) {
          copy.setAttribute('src', value);
        } else if (REMOTE.test(value)) {
          stats.remote += 1;
        } else {
          stats.images += 1;
        }
      } else if (tag === 'IMG' && name === 'alt') {
        copy.setAttribute('alt', value);
      } else if (tag === 'IMG' && name === 'data-cid' && TOKEN.test(value)) {
        copy.setAttribute('data-cid', value);
      } else if (tag === 'IMG' && name === 'data-name') {
        copy.setAttribute('data-name', value.slice(0, 200));
      } else if (tag === 'IMG' && (name === 'width' || name === 'height') && /^\d{1,5}$/.test(value)) {
        copy.setAttribute(name, value);
      } else if ((tag === 'TD' || tag === 'TH') && (name === 'colspan' || name === 'rowspan') && /^\d{1,3}$/.test(value)) {
        copy.setAttribute(name, value);
      } else if (tag === 'OL' && name === 'start' && /^-?\d{1,6}$/.test(value)) {
        copy.setAttribute(name, value);
      } else if (own && tag === 'DIV' && name === 'class' && value.split(/\s+/).includes('sw-signature')) {
        copy.setAttribute('class', 'sw-signature');
      } else if (tag === 'BLOCKQUOTE' && name === 'type' && value.toLowerCase() === 'cite') {
        copy.setAttribute('type', 'cite');
      } else if (tag === 'BLOCKQUOTE' && name === 'style') {
        if (/margin/.test(value) && /40px/.test(value)) {
          copy.setAttribute('style', INDENT_STYLE);
        } else {
          stats.styles += 1;
        }
      } else if (name === 'style') {
        stats.styles += 1;
      } else {
        if (URL_ATTRIBUTE.test(name) && REMOTE.test(value)) {
          stats.remote += 1;
        }
        stats.attributes += 1;
      }
    }
    if (tag === 'IMG') {
      if (!copy.hasAttribute('src')) {
        return null;
      }
      if (!copy.hasAttribute('data-cid') && !copy.getAttribute('src').startsWith('cid:')) {
        copy.setAttribute('data-cid', token());
      }
    }
    return copy;
  };

  const cleanInto = (source, target, doc, stats, depth, own) => {
    if (depth > 64) {
      stats.deep += 1;
      return;
    }
    for (const node of Array.from(source.childNodes)) {
      if (node.nodeType === Node.TEXT_NODE) {
        target.appendChild(doc.createTextNode(node.data));
        continue;
      }
      if (node.nodeType !== Node.ELEMENT_NODE) {
        stats.comments += 1;
        continue;
      }
      const tag = node.tagName.toUpperCase();
      if (DROP.has(tag)) {
        stats.dropped[tag] = (stats.dropped[tag] || 0) + 1;
        countAttributes(node, stats);
        for (const inner of node.querySelectorAll('*')) {
          countAttributes(inner, stats);
        }
        continue;
      }
      let parent = target;
      for (const wrapper of emphasis(node)) {
        if (wrapper !== SEMANTIC[tag]) {
          const element = doc.createElement(wrapper);
          parent.appendChild(element);
          parent = element;
        }
      }
      if ((tag === 'B' || tag === 'STRONG') && normalWeight(node)) {
        stats.unwrapped += 1;
        countAttributes(node, stats);
        cleanInto(node, parent, doc, stats, depth + 1, own);
      } else if (BLOCKS.has(tag)) {
        const block = doc.createElement('DIV');
        countAttributes(node, stats);
        parent.appendChild(block);
        cleanInto(node, block, doc, stats, depth + 1, own);
      } else if (!KEEP.has(tag)) {
        stats.unwrapped += 1;
        countAttributes(node, stats);
        cleanInto(node, parent, doc, stats, depth + 1, own);
      } else {
        const copy = copyElement(node, tag, doc, stats, own);
        if (copy) {
          parent.appendChild(copy);
          if (!VOID.has(tag)) {
            cleanInto(node, copy, doc, stats, depth + 1, own);
          }
        }
      }
    }
  };

  const clean = (html, own) => {
    const doc = new DOMParser().parseFromString(html, 'text/html');
    const stats = emptyStats();
    for (const element of doc.head ? Array.from(doc.head.children) : []) {
      stats.dropped[element.tagName] = (stats.dropped[element.tagName] || 0) + 1;
    }
    const out = doc.createElement('div');
    cleanInto(doc.body, out, doc, stats, 0, !!own);
    return { html: out.innerHTML, stats };
  };

  const escapeAttribute = (value) => value
    .replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

  const readAsDataUrl = (file) => new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.addEventListener('load', () => resolve(reader.result));
    reader.addEventListener('error', () => reject(reader.error));
    reader.readAsDataURL(file);
  });

  // Every data: image the editor holds becomes a part of the draft: its bytes go to Pronto
  // once, which stores them and answers with `adopt`, so the editor shows the image through
  // the composer's cid: scheme from then on.
  const offered = new Set();
  const offerImages = () => {
    for (const image of Array.from(document.querySelectorAll('img[src^="data:"]'))) {
      const match = /^data:(image\/(?:png|jpeg|gif|webp));base64,(.*)$/is.exec(image.getAttribute('src') || '');
      if (!match) {
        image.remove();
        continue;
      }
      let id = image.getAttribute('data-cid');
      if (!id || !TOKEN.test(id)) {
        id = token();
        image.setAttribute('data-cid', id);
      }
      if (offered.has(id)) {
        continue;
      }
      offered.add(id);
      post({
        type: 'image',
        token: id,
        mime: match[1].toLowerCase(),
        data: match[2].replace(/\s+/g, ''),
        name: image.getAttribute('data-name') || '',
      });
    }
  };

  const insertFiles = async (files, report) => {
    for (const file of files) {
      if (!/^image\/(png|jpeg|gif|webp)$/.test(file.type)) {
        if (file.size > MAX_IMAGE) {
          post({ type: 'error', message: 'a dropped file was too large to read here' });
          continue;
        }
        const url = await readAsDataUrl(file);
        post({ type: 'file', name: file.name, mime: file.type, data: String(url).replace(/^data:[^,]*,/, '') });
        report.attachments += 1;
        continue;
      }
      if (file.size > MAX_IMAGE) {
        post({ type: 'error', message: 'an image over 10 MB was not inserted' });
        continue;
      }
      const url = await readAsDataUrl(file);
      const name = file.name ? ` data-name="${escapeAttribute(file.name.slice(0, 200))}"` : '';
      document.execCommand('insertHTML', false, `<img src="${url}" data-cid="${token()}"${name}>`);
      report.images += 1;
    }
    offerImages();
  };

  const insertCleaned = (html, report) => {
    const cleaned = clean(html, false);
    report.stats = cleaned.stats;
    if (cleaned.html) {
      document.execCommand('insertHTML', false, cleaned.html);
      markAttributions();
    }
    offerImages();
  };

  const transferReport = (kind, data) => ({
    type: kind,
    types: Array.from(data.types || []),
    files: (data.files || []).length,
    images: 0,
    attachments: 0,
    mode: 'native',
    stats: null,
  });

  const handleTransfer = (event, data, report) => {
    const files = Array.from(data.files || []);
    if (files.length) {
      event.preventDefault();
      report.mode = 'files';
      insertFiles(files, report).then(() => post(report), (error) => {
        report.mode = `files-failed: ${error}`;
        post(report);
      });
      return;
    }
    const html = report.types.includes('text/html') ? data.getData('text/html') : '';
    if (html) {
      event.preventDefault();
      report.mode = 'html';
      insertCleaned(html, report);
      post(report);
      return;
    }
    const text = data.getData('text/plain');
    if (text) {
      event.preventDefault();
      report.mode = 'text';
      document.execCommand('insertText', false, text);
    }
    post(report);
  };

  document.addEventListener('paste', (event) => {
    if (event.clipboardData) {
      handleTransfer(event, event.clipboardData, transferReport('paste', event.clipboardData));
    }
  }, true);

  // WebKitGTK exposes neither types nor files for an image-only clipboard; its native paste
  // inserts <img src="blob:…">, which the CSP keeps from loading. The helper reads those (the
  // content filter lets only blob:, cid:, data: and about: through) into data: images, which
  // then become parts.
  const adoptBlobImages = async () => {
    for (const image of Array.from(document.querySelectorAll('img[src^="blob:"]'))) {
      try {
        const blob = await (await fetch(image.getAttribute('src'))).blob();
        if (!/^image\/(png|jpeg|gif|webp)$/.test(blob.type) || blob.size > MAX_IMAGE) {
          post({ type: 'error', message: 'a pasted image of another type or over 10 MB was removed' });
          image.remove();
          continue;
        }
        image.setAttribute('src', await readAsDataUrl(blob));
        image.setAttribute('data-cid', token());
      } catch (error) {
        post({ type: 'error', message: 'a pasted image could not be read' });
        image.remove();
      }
    }
    offerImages();
  };
  document.addEventListener('input', (event) => {
    if (event.inputType === 'insertFromPaste' || event.inputType === 'insertFromDrop') {
      adoptBlobImages();
      markAttributions();
    }
  }, true);

  let draggingInside = false;
  document.addEventListener('dragstart', () => { draggingInside = true; }, true);
  document.addEventListener('dragend', () => { draggingInside = false; }, true);
  document.addEventListener('dragover', (event) => {
    if (!draggingInside && event.dataTransfer && Array.from(event.dataTransfer.types).includes('Files')) {
      event.preventDefault();
    }
  }, true);
  document.addEventListener('drop', (event) => {
    const data = event.dataTransfer;
    if (!data || draggingInside) {
      return;
    }
    const range = document.caretRangeFromPoint(event.clientX, event.clientY);
    // Under a folded quote it goes in where a press there puts the caret (`caretBeforeFold`).
    if (range && !(inFold(range.startContainer) && caretBeforeFold())) {
      const selection = document.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
    }
    handleTransfer(event, data, transferReport('drop', data));
  }, true);

  const closestElement = (node, selector) => {
    const element = node && node.nodeType === Node.ELEMENT_NODE ? node : node && node.parentElement;
    return element ? element.closest(selector) : null;
  };

  const BLOCK_NAMES = new Set(['p', 'h1', 'h2', 'h3', 'pre']);
  const computeState = () => {
    const selection = document.getSelection();
    const anchor = selection ? selection.anchorNode : null;
    const list = closestElement(anchor, 'ul, ol');
    const link = closestElement(anchor, 'a[href]');
    const block = String(document.queryCommandValue('formatBlock') || '').toLowerCase();
    return {
      type: 'state',
      bulleted: !!list && list.tagName === 'UL',
      numbered: !!list && list.tagName === 'OL',
      quote: !!closestElement(anchor, 'blockquote:not([style])'),
      indent: !!closestElement(anchor, 'blockquote[style]'),
      link: link ? link.getAttribute('href') : '',
      block: BLOCK_NAMES.has(block) ? block : 'p',
      selection: !!selection && !selection.isCollapsed,
      bold: document.queryCommandState('bold'),
      italic: document.queryCommandState('italic'),
      underline: document.queryCommandState('underline'),
      strikethrough: document.queryCommandState('strikeThrough'),
    };
  };

  let lastState = '';
  let stateScheduled = false;
  const scheduleState = () => {
    if (stateScheduled) {
      return;
    }
    stateScheduled = true;
    setTimeout(() => {
      stateScheduled = false;
      const state = computeState();
      const key = JSON.stringify(state);
      if (key !== lastState) {
        lastState = key;
        post(state);
      }
    }, 0);
  };
  document.addEventListener('selectionchange', scheduleState);
  document.addEventListener('input', scheduleState);

  // WebKit hands the view's keys to the page one at a time, so Send waits until the page has had
  // every key typed before it (the card counts what it gave the view).
  document.addEventListener('keydown', () => {
    keysSeen += 1;
  }, true);

  // Every edit, for the autosave; the text itself never leaves here this way.
  document.addEventListener('input', (event) => {
    post({ type: 'input', inputType: event.inputType, composing: !!event.isComposing });
  }, true);

  // The outgoing HTML: the cleaned document, every image a cid: reference to a part of the
  // draft (an image not yet adopted gets the Content-ID its adoption will give it), quotes
  // marked as citations.
  const serialize = (domain) => {
    const cleaned = clean(document.body.innerHTML, true);
    const doc = new DOMParser().parseFromString('<!DOCTYPE html><body></body>', 'text/html');
    const root = doc.body;
    root.innerHTML = cleaned.html;
    const cids = [];
    for (const image of Array.from(root.querySelectorAll('img'))) {
      const source = image.getAttribute('src') || '';
      const id = image.getAttribute('data-cid');
      if (source.startsWith('data:') && id && TOKEN.test(id)) {
        image.setAttribute('src', `cid:${id}@${domain}`);
      } else if (!source.startsWith('cid:')) {
        image.remove();
        continue;
      }
      image.removeAttribute('data-cid');
      image.removeAttribute('data-name');
      cids.push(image.getAttribute('src').slice(4));
    }
    for (const quote of Array.from(root.querySelectorAll('blockquote'))) {
      if (!quote.hasAttribute('style')) {
        quote.setAttribute('type', 'cite');
        quote.setAttribute('style', QUOTE_STYLE);
      }
    }
    return { html: root.innerHTML, cids, stats: cleaned.stats };
  };

  const textNodes = () => {
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const nodes = [];
    while (walker.nextNode()) {
      nodes.push(walker.currentNode);
    }
    return nodes;
  };

  const select = (needle, collapse, offset) => {
    for (const node of textNodes()) {
      const at = node.data.indexOf(needle);
      if (at >= 0) {
        const range = document.createRange();
        if (collapse) {
          range.setStart(node, at + offset);
          range.collapse(true);
        } else {
          range.setStart(node, at);
          range.setEnd(node, at + needle.length);
        }
        const selection = document.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        return true;
      }
    }
    return false;
  };

  // Attribution lines and the folded quote (the fork's phase C). The marks below are attributes
  // on the body's own nodes, which the cleaner keeps none of: the draft never holds them.
  //
  // An attribution line ("On Mon, 5 Oct 2026 at 10:00, Grace <grace@…> wrote:") is
  // domain::quotes's: at most 300 characters ending with ':' that name a "wrote" verb as a word.
  const WROTE = ['wrote', 'skrev', 'schrieb', 'schreef', 'a écrit', 'escribió', 'ha scritto',
    'escreveu', 'kirjoitti', 'napisał', 'napisała', 'написал'];
  const WORD = /[\p{L}\p{N}]/u;
  const hasText = (text) => /[^\s\u200b-\u200d]/.test(text || '');
  const isAttribution = (element) => {
    const line = (element.textContent || '').trim();
    if (!line.endsWith(':') || line.length > 600 || Array.from(line).length > 300 || line.startsWith('>')) {
      return false;
    }
    const lower = line.toLowerCase();
    return WROTE.some((verb) => {
      for (let at = lower.indexOf(verb); at >= 0; at = lower.indexOf(verb, at + 1)) {
        const before = Array.from(lower.slice(Math.max(0, at - 2), at)).pop() || '';
        const after = Array.from(lower.slice(at + verb.length, at + verb.length + 2))[0] || '';
        if (!WORD.test(before) && !WORD.test(after) && after !== '-') {
          return true;
        }
      }
      return false;
    });
  };
  // The attribution line just before `quote` (blank text may sit between), if that is one.
  const lineBefore = (quote) => {
    let line = quote.previousSibling;
    while (line && line.nodeType !== Node.ELEMENT_NODE) {
      if (line.nodeType === Node.TEXT_NODE && hasText(line.data)) {
        return null;
      }
      line = line.previousSibling;
    }
    return line && !line.matches('blockquote, div.sw-signature') && isAttribution(line) ? line : null;
  };

  // Only attribution lines read small and muted (the editor's stylesheet, the approved mock's):
  // the user's own line before a quote, such as an answer between a quote's parts, keeps the
  // body's size (the stylesheet's `:has(+ blockquote)` took any line). Marked when content comes
  // in (set, pasted); a marked line that stops reading as one (typed into, or the empty line
  // Enter splits off it, which WebKit makes as a copy) loses its mark.
  const ATTRIBUTION = 'data-sw-attribution';
  const marked = new Set();
  const markAttributions = () => {
    for (const quote of Array.from(document.body.querySelectorAll('blockquote:not([style]), blockquote[type=cite]'))) {
      const line = lineBefore(quote);
      if (line) {
        line.setAttribute(ATTRIBUTION, '');
        marked.add(line);
      }
    }
  };

  // A reply's quoted history folded behind a quiet "•••" pill, as the reader folds it: in a card
  // that answers a message (`setContent`'s `foldable`: a reply, a reply's draft reopened), the
  // last quote when nothing but blank lines or the signature follows it, with its attribution
  // line, which becomes the pill (its own ::before, the editor's stylesheet). A quote that the
  // user's own text follows (an answer between the quote's parts) or that has no attribution
  // line stays open, as do a new message's and a forward's: nothing but the attribution is ever
  // a pill, and there is one. A click on the pill, or the caret going onto it (the arrow keys,
  // Ctrl+End, Select All), shows the quote for the rest of the card's life, as does an edit that
  // leaves it no longer the trailing quote. The quote stays in the draft and is sent.
  const FOLD = 'data-sw-fold';
  const FOLDED = 'sw-quote-folded';
  let fold = null;
  const folded = () => fold !== null;
  const blank = (node) => {
    if (node.nodeType === Node.TEXT_NODE) {
      return !hasText(node.data);
    }
    if (node.nodeType !== Node.ELEMENT_NODE || node.matches('div.sw-signature')) {
      return true;
    }
    return !hasText(node.textContent) && !node.matches('img, hr') && !node.querySelector('img, hr');
  };
  const trailing = (quote) => {
    for (let at = quote; at && at !== document.body; at = at.parentNode) {
      for (let next = at.nextSibling; next; next = next.nextSibling) {
        if (!blank(next)) {
          return false;
        }
      }
    }
    return true;
  };
  const foldQuote = () => {
    const quotes = Array.from(document.body.querySelectorAll('blockquote[type=cite]'))
      .filter((quote) => !quote.parentElement.closest('blockquote[type=cite]'));
    const quote = quotes[quotes.length - 1];
    const attribution = quote && trailing(quote) ? lineBefore(quote) : null;
    if (!attribution) {
      return;
    }
    attribution.setAttribute(FOLD, 'pill');
    quote.setAttribute(FOLD, 'quote');
    fold = { attribution, quote };
    document.documentElement.classList.add(FOLDED);
  };
  const unfold = () => {
    if (!fold) {
      return;
    }
    fold = null;
    document.documentElement.classList.remove(FOLDED);
    for (const element of Array.from(document.querySelectorAll(`[${FOLD}]`))) {
      element.removeAttribute(FOLD);
    }
  };
  const inFold = (node) => !!fold && !!node && (fold.quote.contains(node) || fold.attribution.contains(node));
  document.addEventListener('click', (event) => {
    if (inFold(event.target)) {
      event.preventDefault();
      unfold();
    }
  }, true);
  // A press beside or under the pill (the body's empty space under the text) would put the
  // caret on the pill's line and open the quote: it goes to the end of the line before the fold
  // instead, where it would go with no quote under the text. Only the pill itself opens it.
  const caretBeforeFold = () => {
    let at = fold.attribution;
    let line = null;
    while (at && at !== document.body && !line) {
      line = at.previousSibling;
      while (line && (line.nodeType === Node.COMMENT_NODE || (line.nodeType === Node.TEXT_NODE && !hasText(line.data)))) {
        line = line.previousSibling;
      }
      at = at.parentNode;
    }
    if (!line) {
      return false;
    }
    const selection = document.getSelection();
    if (line.nodeType === Node.TEXT_NODE) {
      selection.collapse(line, line.length);
    } else {
      selection.selectAllChildren(line);
      selection.collapseToEnd();
    }
    return true;
  };
  document.addEventListener('mousedown', (event) => {
    if (!fold || event.button !== 0 || inFold(event.target)) {
      return;
    }
    const range = document.caretRangeFromPoint(event.clientX, event.clientY);
    if (range && inFold(range.startContainer) && caretBeforeFold()) {
      event.preventDefault();
    }
  }, true);
  document.addEventListener('selectionchange', () => {
    const selection = document.getSelection();
    if (fold && selection && (inFold(selection.anchorNode) || inFold(selection.focusNode))) {
      unfold();
    }
  });
  // After every edit: the marks of lines that no longer read as attributions go, and the fold
  // opens once its quote isn't the trailing one under its attribution any more.
  new MutationObserver((records) => {
    for (const record of records) {
      for (const node of record.addedNodes) {
        if (node.nodeType !== Node.ELEMENT_NODE) {
          continue;
        }
        if (node.hasAttribute(ATTRIBUTION)) {
          marked.add(node);
        }
        for (const inner of node.querySelectorAll(`[${ATTRIBUTION}]`)) {
          marked.add(inner);
        }
      }
    }
    for (const line of Array.from(marked)) {
      if (!line.isConnected) {
        // Kept marked: Undo may bring it back (then an added node again).
        marked.delete(line);
      } else if (!isAttribution(line)) {
        line.removeAttribute(ATTRIBUTION);
        marked.delete(line);
      }
    }
    if (fold && !(fold.quote.isConnected && lineBefore(fold.quote) === fold.attribution && trailing(fold.quote))) {
      unfold();
    }
  }).observe(document.body, { childList: true, characterData: true, subtree: true });
  // What a folded quote says too: the body's text is the same folded or not.
  const allText = () => {
    if (!folded()) {
      return document.body.innerText;
    }
    document.documentElement.classList.remove(FOLDED);
    const text = document.body.innerText;
    document.documentElement.classList.add(FOLDED);
    return text;
  };

  // The body's top-level nodes before the signature and the quote (with its attribution line),
  // and those after them.
  const typedParts = () => {
    const signature = document.querySelector('body > div.sw-signature');
    const quote = document.querySelector('body > blockquote[type=cite]');
    const attribution = quote ? quote.previousElementSibling : null;
    const stops = [signature, attribution, quote].filter(Boolean);
    const nodes = Array.from(document.body.childNodes);
    const first = stops.length ? Math.min(...stops.map((stop) => nodes.indexOf(stop))) : nodes.length;
    const last = stops.length ? Math.max(...stops.map((stop) => nodes.indexOf(stop))) : nodes.length;
    return { typed: nodes.slice(0, first), after: nodes.slice(last + 1) };
  };

  window.sealwaxComposer = {
    serialize,
    clean: (html) => clean(html, false),
    state: computeState,
    html: () => document.body.innerHTML,
    text: allText,
    isEmpty: () => !allText().trim() && !document.querySelector('img'),
    quoteFolded: folded,
    images: () => Array.from(document.querySelectorAll('img')).map((image) => image.getAttribute('src') || ''),
    loadedImages: () => Array.from(document.querySelectorAll('img'))
      .filter((image) => image.complete && image.naturalWidth > 0).length,
    insertText: (text) => document.execCommand('insertText', false, text),
    keysSeen: () => keysSeen,
    // The sender's signature in place of the one shown (its HTML comes from the compose
    // rules, cleaned here like everything else); before the quote when there is none yet.
    setSignature: (html) => {
      const current = document.querySelector('div.sw-signature');
      const cleaned = clean(html, true).html;
      if (current) {
        if (cleaned) {
          current.outerHTML = cleaned;
        } else {
          current.remove();
        }
        return true;
      }
      if (!cleaned) {
        return false;
      }
      const quote = document.querySelector('blockquote[type=cite]');
      const forwarded = Array.from(document.body.children).find((node) =>
        node.tagName === 'DIV' && node.textContent === '---------- Forwarded message ----------');
      const anchor = quote && quote.previousElementSibling ? quote.previousElementSibling : (quote || forwarded);
      const holder = document.createElement('div');
      holder.innerHTML = `<div><br></div>${cleaned}`;
      for (const node of Array.from(holder.childNodes)) {
        if (anchor) {
          anchor.parentNode.insertBefore(node, anchor);
        } else {
          document.body.appendChild(node);
        }
      }
      return true;
    },
    insertImage: (cid, name) => document.execCommand('insertHTML', false,
      `<img src="cid:${escapeAttribute(cid)}" alt="${escapeAttribute(name.slice(0, 200))}">`),
    // A link on the selection, or the address itself as a link where nothing is selected.
    link: (url) => {
      if (!LINK.test(url)) {
        return false;
      }
      const selection = document.getSelection();
      if (selection && !selection.isCollapsed) {
        return document.execCommand('createLink', false, url);
      }
      const text = url.replace(/^mailto:/i, '');
      return document.execCommand('insertHTML', false,
        `<a href="${escapeAttribute(url)}">${escapeAttribute(text)}</a>`);
    },
    insertCleaned: (html) => {
      const report = { stats: null };
      insertCleaned(html, report);
      return report.stats;
    },
    // `foldable`: the draft answers a message, so its quoted history may fold.
    setContent: (html, foldable) => {
      unfold();
      marked.clear();
      document.body.innerHTML = html || '<div><br></div>';
      markAttributions();
      if (foldable) {
        foldQuote();
      }
      offerImages();
      lastState = '';
      scheduleState();
    },
    adopt: (id, cid) => {
      let adopted = 0;
      for (const image of Array.from(document.querySelectorAll('img[data-cid]'))) {
        if (image.getAttribute('data-cid') === id) {
          image.setAttribute('src', `cid:${cid}`);
          image.removeAttribute('data-cid');
          adopted += 1;
        }
      }
      return adopted;
    },
    selectText: (needle) => select(needle, false, 0),
    selectLink: () => {
      const selection = document.getSelection();
      const link = closestElement(selection ? selection.anchorNode : null, 'a[href]');
      if (!link) {
        return false;
      }
      const range = document.createRange();
      range.selectNodeContents(link);
      selection.removeAllRanges();
      selection.addRange(range);
      return true;
    },
    caretAt: (needle, offset) => select(needle, true, offset),
    caretAtStart: () => {
      const selection = document.getSelection();
      selection.selectAllChildren(document.body);
      selection.collapseToStart();
      return true;
    },
    caretAtEnd: () => {
      const selection = document.getSelection();
      selection.selectAllChildren(document.body);
      selection.collapseToEnd();
      return true;
    },
    // What the user typed above the signature and the quote (the fork's phase B, Pop Draft Out
    // or In): its lines, and whether it is plain text (lines of text only) with nothing typed
    // below them.
    typed: () => {
      const parts = typedParts();
      let plain = parts.after.every((node) => !(node.textContent || '').trim() &&
        !(node.querySelector && node.querySelector('img')));
      const lines = [];
      for (const node of parts.typed) {
        if (node.nodeType === Node.TEXT_NODE) {
          lines.push(node.textContent);
          continue;
        }
        if (node.nodeType !== Node.ELEMENT_NODE) {
          continue;
        }
        const simple = node.tagName === 'DIV' && Array.from(node.childNodes).every((child) =>
          child.nodeType === Node.TEXT_NODE || child.tagName === 'BR');
        if (!simple) {
          plain = false;
        }
        lines.push((node.innerText || '').replace(/\n$/, ''));
      }
      while (lines.length && !lines[lines.length - 1].trim()) {
        lines.pop();
      }
      const signature = document.querySelector('div.sw-signature');
      if (signature && Array.from(signature.querySelectorAll('*')).some((node) =>
        !['DIV', 'BR'].includes(node.tagName))) {
        plain = false;
      }
      const signatureText = signature ? (signature.innerText || '')
        .replace(/^--[ \u00a0]\n?/, '').replace(/\n$/, '') : '';
      return { text: lines.join('\n'), plain, signature: signatureText };
    },
    // A snippet at the caret (Use Snippet…, the fork's phase B): its text typed in over the
    // selection (at the body's start when it has no caret yet, a new message's), then the caret
    // put back before `after`, the text after its {cursor}: one step back per grapheme, as
    // selection.modify moves ('👍🏽' is one step, two code points; a line break is one).
    insertSnippet: (text, after) => {
      const selection = document.getSelection();
      if (!selection.rangeCount || !document.body.contains(selection.anchorNode)) {
        selection.selectAllChildren(document.body);
        selection.collapseToStart();
      }
      document.body.focus();
      document.execCommand('insertText', false, text);
      const back = typeof Intl.Segmenter === 'function'
        ? [...new Intl.Segmenter(undefined, { granularity: 'grapheme' }).segment(after)].length
        : Array.from(after).length;
      for (let step = 0; step < back; step += 1) {
        selection.modify('move', 'backward', 'character');
      }
      return true;
    },
    // The caret after what the user typed (a reply popped out of the inline reply).
    caretAfterTyped: () => {
      const typed = typedParts().typed.filter((node) => (node.textContent || '').trim());
      const last = typed[typed.length - 1];
      const selection = document.getSelection();
      if (!last) {
        selection.selectAllChildren(document.body);
        selection.collapseToStart();
        return false;
      }
      selection.selectAllChildren(last);
      selection.collapseToEnd();
      return true;
    },
    testDrop: (name, mime, base64) => {
      const bytes = Uint8Array.from(atob(base64), (char) => char.charCodeAt(0));
      const transfer = new DataTransfer();
      transfer.items.add(new File([bytes], name, { type: mime }));
      const rect = document.body.getBoundingClientRect();
      const event = new DragEvent('drop', {
        bubbles: true,
        cancelable: true,
        dataTransfer: transfer,
        clientX: rect.left + 10,
        clientY: rect.top + 10,
      });
      document.body.dispatchEvent(event);
      return event.defaultPrevented;
    },
  };

  post({ type: 'ready', editable: document.body ? document.body.isContentEditable : false });
  scheduleState();
})();
