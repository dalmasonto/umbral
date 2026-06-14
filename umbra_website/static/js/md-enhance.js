/*
 * md-enhance.js — progressive enhancement for rendered Markdown.
 *
 * The framework's `| markdown` filter emits plain `<pre><code>` blocks and
 * `<img>` tags (and we don't touch the filter — it lives in the framework
 * crate, outside this project). This script enhances that output IN THE
 * BROWSER, on any container marked `[data-md]`:
 *
 *   1. Code blocks  → a header bar with the language label + a Copy button,
 *      and a real "code preview" frame (the .md-code wrapper).
 *   2. Images       → click to open a lightbox; multiple images across the
 *      page form a gallery with prev/next + keyboard nav.
 *
 * No dependencies, no build step. Degrades to plain markdown if JS is off.
 */
(function () {
  "use strict";

  function ready(fn) {
    if (document.readyState !== "loading") fn();
    else document.addEventListener("DOMContentLoaded", fn);
  }

  ready(function () {
    var roots = Array.prototype.slice.call(document.querySelectorAll("[data-md]"));
    if (!roots.length) return;

    var gallery = []; // every enhanced image, in document order
    roots.forEach(function (root) {
      enhanceCodeBlocks(root);
      collectImages(root, gallery);
    });
    if (gallery.length) initLightbox(gallery);
  });

  /* ---- 1. Code blocks: language label + copy + preview frame ---------- */
  function enhanceCodeBlocks(root) {
    Array.prototype.slice.call(root.querySelectorAll("pre")).forEach(function (pre) {
      if (pre.parentNode && pre.parentNode.classList.contains("md-code")) return;
      var code = pre.querySelector("code");

      var lang = "";
      if (code) {
        var m = (code.className || "").match(/language-([\w+-]+)/);
        if (m) lang = m[1];
      }

      var wrap = document.createElement("div");
      // `not-prose` opts the frame out of Tailwind `prose` styling so the
      // dark code frame owns its look (prose would otherwise re-style the
      // <pre>/<code> inside it).
      wrap.className = "md-code not-prose";

      var bar = document.createElement("div");
      bar.className = "md-code__bar";

      var label = document.createElement("span");
      label.className = "md-code__lang";
      label.textContent = lang || "code";

      var btn = document.createElement("button");
      btn.type = "button";
      btn.className = "md-code__copy";
      btn.setAttribute("aria-label", "Copy code");
      btn.innerHTML = copyIcon() + '<span class="md-code__copy-text">Copy</span>';
      btn.addEventListener("click", function () {
        var text = code ? code.innerText : pre.innerText;
        var done = function () {
          btn.classList.add("is-copied");
          var t = btn.querySelector(".md-code__copy-text");
          if (t) t.textContent = "Copied";
          setTimeout(function () {
            btn.classList.remove("is-copied");
            if (t) t.textContent = "Copy";
          }, 1500);
        };
        if (navigator.clipboard && navigator.clipboard.writeText) {
          navigator.clipboard.writeText(text).then(done, fallbackCopy.bind(null, text, done));
        } else {
          fallbackCopy(text, done);
        }
      });

      bar.appendChild(label);
      bar.appendChild(btn);

      pre.parentNode.insertBefore(wrap, pre);
      wrap.appendChild(bar);
      wrap.appendChild(pre);
    });
  }

  function fallbackCopy(text, done) {
    try {
      var ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
      done();
    } catch (_) {
      /* clipboard unavailable — leave the code selectable */
    }
  }

  /* ---- 2. Images: clickable lightbox + gallery ----------------------- */
  function collectImages(root, gallery) {
    Array.prototype.slice.call(root.querySelectorAll("img")).forEach(function (img) {
      img.classList.add("md-img");
      var idx = gallery.length;
      gallery.push(img);
      img.addEventListener("click", function () { openLightbox(gallery, idx); });
      img.setAttribute("role", "button");
      img.setAttribute("tabindex", "0");
      img.addEventListener("keydown", function (e) {
        if (e.key === "Enter" || e.key === " ") { e.preventDefault(); openLightbox(gallery, idx); }
      });
    });
  }

  var lb = null; // the single overlay, lazily built
  var lbState = { gallery: [], index: 0 };

  function initLightbox(gallery) {
    lbState.gallery = gallery;
    if (lb) return;
    lb = document.createElement("div");
    lb.className = "md-lightbox";
    lb.setAttribute("aria-hidden", "true");
    lb.innerHTML =
      '<button class="md-lightbox__close" aria-label="Close">✕</button>' +
      '<button class="md-lightbox__nav md-lightbox__prev" aria-label="Previous">‹</button>' +
      '<figure class="md-lightbox__figure"><img class="md-lightbox__img" alt=""><figcaption class="md-lightbox__cap"></figcaption></figure>' +
      '<button class="md-lightbox__nav md-lightbox__next" aria-label="Next">›</button>';
    document.body.appendChild(lb);

    lb.querySelector(".md-lightbox__close").addEventListener("click", closeLightbox);
    lb.querySelector(".md-lightbox__prev").addEventListener("click", function (e) { e.stopPropagation(); step(-1); });
    lb.querySelector(".md-lightbox__next").addEventListener("click", function (e) { e.stopPropagation(); step(1); });
    lb.addEventListener("click", function (e) { if (e.target === lb) closeLightbox(); });
    document.addEventListener("keydown", function (e) {
      if (lb.getAttribute("aria-hidden") === "true") return;
      if (e.key === "Escape") closeLightbox();
      else if (e.key === "ArrowLeft") step(-1);
      else if (e.key === "ArrowRight") step(1);
    });
  }

  function openLightbox(gallery, index) {
    initLightbox(gallery);
    lbState.gallery = gallery;
    lbState.index = index;
    render();
    lb.setAttribute("aria-hidden", "false");
    document.body.style.overflow = "hidden";
  }

  function closeLightbox() {
    if (!lb) return;
    lb.setAttribute("aria-hidden", "true");
    document.body.style.overflow = "";
  }

  function step(delta) {
    var n = lbState.gallery.length;
    lbState.index = (lbState.index + delta + n) % n;
    render();
  }

  function render() {
    var img = lbState.gallery[lbState.index];
    if (!img) return;
    var full = img.getAttribute("data-full") || img.currentSrc || img.src;
    var cap = img.getAttribute("alt") || "";
    lb.querySelector(".md-lightbox__img").src = full;
    lb.querySelector(".md-lightbox__img").alt = cap;
    lb.querySelector(".md-lightbox__cap").textContent = cap;
    var multi = lbState.gallery.length > 1;
    lb.querySelector(".md-lightbox__prev").style.display = multi ? "" : "none";
    lb.querySelector(".md-lightbox__next").style.display = multi ? "" : "none";
  }

  function copyIcon() {
    return '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
  }
})();
