/* Collapsible hierarchical sidebar for Asciidoctor `toc: left` output.
 * No dependencies. Progressively enhanced: without JS the TOC stays
 * fully expanded and every anchor link works normally.
 *
 * Behaviour:
 * - A disclosure <button> is added to each TOC entry that has children.
 *   Title links navigate; only the button toggles collapse.
 * - The tree starts minimized: on a first visit every collapsible entry
 *   is collapsed so only the major chapters show (RISC-V manual style
 *   chapter list, but collapsed instead of expanded).
 * - Collapsed hrefs persist in localStorage (gracefully ignored if
 *   storage is unavailable); return visits restore the last state.
 * - The hierarchy containing location.hash is auto-expanded on load.
 * - A scroll-spy highlights the current section and expands its ancestors.
 * - "Collapse all" / "Expand all" controls sit under the TOC title.
 */
(function () {
  'use strict';

  var STORAGE_KEY = 'docs-nav-collapsed-v1';
  var ACTIVE_CLASS = 'toc-active';
  var COLLAPSED_CLASS = 'nav-collapsed';

  // Returns null on a first visit (nothing saved yet) so the caller can
  // apply the minimized default; otherwise the saved href list
  // (possibly empty after "Expand all").
  function loadCollapsed() {
    try {
      var raw = window.localStorage.getItem(STORAGE_KEY);
      if (raw == null) return null;
      var parsed = JSON.parse(raw);
      return Array.isArray(parsed) ? parsed : [];
    } catch (e) {
      return null;
    }
  }

  function saveCollapsed(hrefs) {
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(hrefs));
    } catch (e) {
      /* storage unavailable (private mode, disabled): ignore */
    }
  }

  function tocLinkHref(li) {
    var a = li.querySelector(':scope > a');
    return a ? a.getAttribute('href') : null;
  }

  function setCollapsed(li, collapsed, persist) {
    var btn = li.querySelector(':scope > button.nav-toggle');
    if (collapsed) {
      li.classList.add(COLLAPSED_CLASS);
    } else {
      li.classList.remove(COLLAPSED_CLASS);
    }
    if (btn) btn.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
    if (btn) btn.textContent = collapsed ? '\u25B8' : '\u25BE';
    if (persist) persist();
  }

  function expandAncestors(link) {
    var li = link.closest('li');
    while (li) {
      li.classList.remove(COLLAPSED_CLASS);
      var btn = li.querySelector(':scope > button.nav-toggle');
      if (btn) {
        btn.setAttribute('aria-expanded', 'true');
        btn.textContent = '\u25BE';
      }
      // climb: li -> ul -> parent li
      var parentUl = li.parentElement;
      li = parentUl ? parentUl.closest('li') : null;
    }
  }

  function currentCollapsedHrefs(toc) {
    var out = [];
    toc.querySelectorAll('li.' + COLLAPSED_CLASS).forEach(function (li) {
      var href = tocLinkHref(li);
      if (href) out.push(href);
    });
    return out;
  }

  function init() {
    var toc = document.getElementById('toc');
    if (!toc) return;
    var tocTitle = document.getElementById('toctitle');
    var collapsed = loadCollapsed();
    var persist = function () { saveCollapsed(currentCollapsedHrefs(toc)); };

    /* Controls: inserted after the TOC title, before the list. */
    var controls = document.createElement('div');
    controls.className = 'nav-controls';
    var collapseAll = document.createElement('button');
    collapseAll.type = 'button';
    collapseAll.textContent = 'Collapse all';
    var expandAll = document.createElement('button');
    expandAll.type = 'button';
    expandAll.textContent = 'Expand all';
    controls.appendChild(collapseAll);
    controls.appendChild(expandAll);
    if (tocTitle && tocTitle.parentNode === toc) {
      tocTitle.after(controls);
    } else {
      toc.prepend(controls);
    }

    /* Add disclosure buttons to every entry with a child list. */
    var items = toc.querySelectorAll('li');
    items.forEach(function (li) {
      var childList = li.querySelector(':scope > ul');
      if (!childList) return;
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'nav-toggle';
      btn.setAttribute('aria-expanded', 'true');
      btn.setAttribute('aria-label', 'Toggle subsection');
      btn.textContent = '\u25BE';
      var link = li.querySelector(':scope > a');
      if (link) link.before(btn);
      else li.prepend(btn);
      btn.addEventListener('click', function () {
        setCollapsed(li, !li.classList.contains(COLLAPSED_CLASS), persist);
      });
    });

    /* Initial state: restore saved state, or start minimized on a
       first visit so only the major chapters show. */
    if (collapsed === null) {
      toc.querySelectorAll('li').forEach(function (li) {
        if (li.querySelector(':scope > ul')) setCollapsed(li, true, null);
      });
      persist();
    } else if (collapsed.length) {
      toc.querySelectorAll('li').forEach(function (li) {
        var href = tocLinkHref(li);
        if (href && collapsed.indexOf(href) !== -1) {
          setCollapsed(li, true, null);
        }
      });
    }

    collapseAll.addEventListener('click', function () {
      toc.querySelectorAll('li').forEach(function (li) {
        if (li.querySelector(':scope > ul')) setCollapsed(li, true, null);
      });
      persist();
    });
    expandAll.addEventListener('click', function () {
      toc.querySelectorAll('li').forEach(function (li) {
        setCollapsed(li, false, null);
      });
      persist();
    });

    /* Auto-expand the hierarchy containing the current hash. */
    function expandForHash() {
      var hash = window.location.hash;
      if (!hash) return;
      var link = toc.querySelector('a[href="' + hash + '"]');
      if (link) {
        expandAncestors(link);
        persist();
      }
    }
    expandForHash();
    window.addEventListener('hashchange', expandForHash);

    /* Scroll-spy: highlight current section, keep it visible in the nav. */
    var tocLinks = Array.prototype.slice.call(toc.querySelectorAll('a[href^="#"]'));
    if (!('IntersectionObserver' in window) || !tocLinks.length) return;
    var headings = tocLinks
      .map(function (a) { return document.querySelector(a.getAttribute('href')); })
      .filter(function (el) { return el && /^H[1-4]$/.test(el.tagName); });
    if (!headings.length) return;

    var activeLink = null;
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) return;
        var id = '#' + entry.target.id;
        var link = toc.querySelector('a[href="' + id + '"]');
        if (!link || link === activeLink) return;
        if (activeLink) activeLink.classList.remove(ACTIVE_CLASS);
        activeLink = link;
        activeLink.classList.add(ACTIVE_CLASS);
        expandAncestors(activeLink);
      });
    }, { rootMargin: '-20% 0px -65% 0px', threshold: 0 });
    headings.forEach(function (h) { observer.observe(h); });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
