// Theme toggle, mobile sidebar and client-side search for the LazyOS docs.
(function () {
  "use strict";
  var root = document.documentElement;

  function safe(fn) { try { return fn(); } catch (e) { return null; } }

  var themeButton = document.getElementById("theme-toggle");
  if (themeButton) {
    themeButton.addEventListener("click", function () {
      var dark = root.getAttribute("data-theme") === "dark" ||
        (!root.getAttribute("data-theme") && window.matchMedia("(prefers-color-scheme: dark)").matches);
      var next = dark ? "light" : "dark";
      root.setAttribute("data-theme", next);
      safe(function () { localStorage.setItem("lazyos-docs-theme", next); });
    });
  }

  var menuButton = document.getElementById("menu-toggle");
  if (menuButton) {
    menuButton.addEventListener("click", function () {
      var open = document.body.classList.toggle("nav-open");
      menuButton.setAttribute("aria-expanded", open ? "true" : "false");
    });
  }

  var sidebar = document.querySelector(".sidebar");
  var current = sidebar && sidebar.querySelector('a[aria-current="page"]');
  if (current) { sidebar.scrollTop = Math.max(0, current.offsetTop - sidebar.clientHeight / 2); }

  // ---- search ----------------------------------------------------------
  var input = document.getElementById("search");
  var list = document.getElementById("search-results");
  if (!input || !list) { return; }
  var script = document.currentScript;
  var base = new URL("../", script.src);
  var docs = null;
  var loading = false;

  function load(done) {
    if (docs) { return done(); }
    if (loading) { return; }
    loading = true;
    fetch(new URL("site-assets/search-index.json", base)).then(function (r) { return r.json(); })
      .then(function (data) { docs = data; done(); })
      .catch(function () { list.textContent = "Search is unavailable."; list.hidden = false; });
  }

  function terms(q) { return q.toLowerCase().split(/[^a-z0-9_.:]+/).filter(Boolean); }

  function score(doc, words) {
    var total = 0;
    for (var i = 0; i < words.length; i++) {
      var w = words[i], s = 0;
      if (doc.t.toLowerCase().indexOf(w) >= 0) { s += 10; }
      if (doc.h.toLowerCase().indexOf(w) >= 0) { s += 4; }
      if (doc.u.toLowerCase().indexOf(w) >= 0) { s += 3; }
      if (doc.x.toLowerCase().indexOf(w) >= 0) { s += 1; }
      if (!s) { return 0; }
      total += s;
    }
    return total;
  }

  function snippet(doc, words) {
    var lower = doc.x.toLowerCase();
    for (var i = 0; i < words.length; i++) {
      var at = lower.indexOf(words[i]);
      if (at >= 0) {
        var from = Math.max(0, at - 50);
        return (from ? "…" : "") + doc.x.slice(from, at + 110) + "…";
      }
    }
    return doc.s;
  }

  function show() {
    var words = terms(input.value);
    list.textContent = "";
    if (!words.length) { list.hidden = true; return; }
    var hits = [];
    docs.forEach(function (d) { var s = score(d, words); if (s) { hits.push([s, d]); } });
    hits.sort(function (a, b) { return b[0] - a[0] || a[1].t.localeCompare(b[1].t); });
    hits.slice(0, 12).forEach(function (hit) {
      var d = hit[1];
      var li = document.createElement("li");
      var a = document.createElement("a");
      a.href = new URL(d.u, base).href;
      var title = document.createElement("strong");
      title.textContent = d.t;
      var meta = document.createElement("span");
      meta.className = "meta";
      meta.textContent = d.g;
      var text = document.createElement("span");
      text.className = "snip";
      text.textContent = snippet(d, words);
      a.appendChild(title); a.appendChild(meta); a.appendChild(text);
      li.appendChild(a);
      list.appendChild(li);
    });
    if (!hits.length) {
      var none = document.createElement("li");
      none.className = "none";
      none.textContent = "No pages match.";
      list.appendChild(none);
    }
    list.hidden = false;
  }

  input.addEventListener("focus", function () { load(function () {}); });
  input.addEventListener("input", function () { load(show); });
  input.addEventListener("keydown", function (e) {
    if (e.key === "Escape") { input.value = ""; list.hidden = true; input.blur(); }
    if (e.key === "Enter") {
      var first = list.querySelector("a");
      if (first) { window.location.href = first.href; }
    }
  });
  document.addEventListener("keydown", function (e) {
    var tag = (e.target && e.target.tagName) || "";
    if (e.key === "/" && tag !== "INPUT" && tag !== "TEXTAREA") { e.preventDefault(); input.focus(); }
  });
  document.addEventListener("click", function (e) {
    if (!list.contains(e.target) && e.target !== input) { list.hidden = true; }
  });
})();
