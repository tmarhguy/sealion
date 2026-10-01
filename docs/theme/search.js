/* Tiny offline search for the static manual. No dependencies, no backend.
 * Indexes section headings + the first paragraph of each section at load,
 * filters the sidebar TOC live, and shows up to 12 jump-to results.
 * Without JS no search box is injected, so the page degrades gracefully.
 *
 * This searches the *documentation*. Searching a live dataset with the
 * project's own engine (HTTP API + product UI) is a separate workstream
 * documented in the manual itself.
 */
(function () {
  'use strict';

  var MAX_RESULTS = 12;

  function sectionText(heading) {
    // First meaningful paragraph after the heading's section container.
    var node = heading.parentElement;
    var depth = 0;
    while (node && node !== document.body && depth < 4) {
      var p = node.querySelector(':scope .paragraph p, :scope p');
      if (p && p.textContent.trim()) return p.textContent.trim().slice(0, 160);
      node = node.nextElementSibling;
      depth += 1;
    }
    return '';
  }

  function init() {
    var toc = document.getElementById('toc');
    if (!toc) return;
    var links = Array.prototype.slice.call(toc.querySelectorAll('a[href^="#"]'));
    if (!links.length) return;

    // Build the box (JS-only so no-JS pages show no dead input).
    var box = document.createElement('div');
    box.className = 'docs-search';
    var input = document.createElement('input');
    input.type = 'search';
    input.placeholder = 'Search this manual… ( / )';
    input.setAttribute('aria-label', 'Search this manual (shortcut: /)');
    input.autocomplete = 'off';
    box.appendChild(input);
    var results = document.createElement('ul');
    results.className = 'docs-search-results';
    results.hidden = true;
    box.appendChild(results);
    var controls = toc.querySelector('.nav-controls');
    if (controls) controls.after(box);
    else toc.prepend(box);

    // Index: TOC link text + target heading + snippet.
    var index = links.map(function (a) {
      var href = a.getAttribute('href');
      var target = document.querySelector(href);
      var title = a.textContent.trim();
      var snippet = target ? sectionText(target) : '';
      return {
        a: a, href: href, title: title,
        hay: (title + ' ' + snippet).toLowerCase(), snippet: snippet
      };
    });

    function clearFilter() {
      toc.classList.remove('toc-searching');
      index.forEach(function (e) {
        var li = e.a.closest('li');
        if (li) li.classList.remove('toc-no-match', 'toc-match');
      });
    }

    input.addEventListener('input', function () {
      var q = input.value.trim().toLowerCase();
      results.innerHTML = '';
      if (!q) {
        results.hidden = true;
        clearFilter();
        return;
      }
      var hits = index.filter(function (e) { return e.hay.indexOf(q) !== -1; });
      toc.classList.add('toc-searching');
      index.forEach(function (e) {
        var li = e.a.closest('li');
        if (!li) return;
        var match = e.hay.indexOf(q) !== -1;
        li.classList.toggle('toc-no-match', !match);
        li.classList.toggle('toc-match', match);
        // Reveal matches hidden inside collapsed parents.
        if (match) {
          var p = li.parentElement;
          while (p && p !== toc) {
            var pli = p.closest('li');
            if (pli) pli.classList.remove('toc-no-match');
            p = pli ? pli.parentElement : null;
          }
        }
      });
      if (!hits.length) {
        var empty = document.createElement('div');
        empty.className = 'docs-search-empty';
        empty.textContent = 'No matches in this manual.';
        results.appendChild(empty);
      } else {
        hits.slice(0, MAX_RESULTS).forEach(function (h) {
          var li = document.createElement('li');
          var a = document.createElement('a');
          a.href = h.href;
          a.textContent = h.title;
          if (h.snippet) a.title = h.snippet;
          li.appendChild(a);
          results.appendChild(li);
        });
      }
      results.hidden = false;
    });

    // Esc clears the search.
    input.addEventListener('keydown', function (ev) {
      if (ev.key === 'Escape') {
        input.value = '';
        input.dispatchEvent(new Event('input'));
        input.blur();
      }
    });

    // Docs-site shortcut (RISC-V/Antora style): "/" focuses search.
    // Ignored while typing in any field or with a modifier held.
    document.addEventListener('keydown', function (ev) {
      if (ev.key !== '/' || ev.ctrlKey || ev.metaKey || ev.altKey) return;
      var t = ev.target;
      if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return;
      ev.preventDefault();
      input.focus();
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
