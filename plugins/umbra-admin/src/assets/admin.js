// umbra-admin runtime JS (gaps2 #4).
//
// Pre-fix: ~1080 lines of JS lived inline in wrapper.html across 3
// <script> blocks (lines 500–1178, 1229–1491, 1493–1634). The
// blocks ran at parser-position, but everything they do is "set up
// helpers + register event listeners" — none of it requires
// mid-parse timing. Extracted here as one external file served via
// the framework's StaticFile mechanism (see static_assets.rs).
//
// `umbraAdminBase` is a global set by a small inline bootstrap in
// wrapper.html (`<script>var umbraAdminBase = '{{ admin_base }}';</script>`).
// Every URL-construction site that used to read `{{ admin_base }}`
// now concatenates `umbraAdminBase + '/...'` instead.
//
// Pre-paint code (theme bootstrap, the window.umbra stub) stays
// inline in <head> — moving it here would flash the wrong theme
// during external-script fetch.

  // Extend the early-declared window.umbra stub with the full IIFE exports.
  // The stub was declared in <head> so child-template inline scripts are safe.
  (function() {
    // ----- Ambient CSRF -----
    // htmx requests inherit the X-CSRF-Token header from <body hx-headers>
    // (rendered from the ambient {{ csrf_token }}); raw fetch() writes
    // bypass that inheritance, so they read the (deliberately
    // non-HttpOnly) cookie here instead.
    function csrfHeaders() {
      var m = document.cookie.match(/(?:^|;\s*)umbra_csrf_token=([^;]*)/);
      return m ? { 'X-CSRF-Token': decodeURIComponent(m[1]) } : {};
    }
    // ----- Theme toggle (Bug 4) -----
    // Source of truth: server prefs (data-theme attr set at render time).
    // localStorage mirrors for instant FOUC-free apply on next load.
    function applyTheme(dark) {
      var root = document.documentElement;
      var themeName = dark ? 'dark' : 'light';
      if (dark) {
        root.classList.add('dark');
      } else {
        root.classList.remove('dark');
      }
      root.setAttribute('data-theme', themeName);
      var light = document.getElementById('theme-icon-light');
      var moon  = document.getElementById('theme-icon-dark');
      if (dark) {
        if (light) light.classList.remove('hidden');
        if (moon)  moon.classList.add('hidden');
      } else {
        if (light) light.classList.add('hidden');
        if (moon)  moon.classList.remove('hidden');
      }
    }
    // Initial theme: read from data-theme (server-rendered); fall back to localStorage.
    var serverTheme = document.documentElement.getAttribute('data-theme');
    var storedTheme = localStorage.getItem('umbra-admin-theme');
    var resolvedTheme = storedTheme || serverTheme || 'dark';
    var isDark = resolvedTheme !== 'light';
    applyTheme(isDark);

    function toggleTheme() {
      isDark = !isDark;
      var themeName = isDark ? 'dark' : 'light';
      localStorage.setItem('umbra-admin-theme', themeName);
      applyTheme(isDark);
      queueChartRefresh();
      // Persist to server so the next hard refresh renders the correct class.
      fetch(umbraAdminBase + '/api/prefs', {
        method: 'PUT',
        headers: Object.assign({ 'Content-Type': 'application/json' }, csrfHeaders()),
        body: JSON.stringify({ theme: themeName })
      }).catch(function() { /* ignore; localStorage mirror is sufficient */ });
    }

    // ----- Responsive sidebar -----
    var sidebarCollapsed = localStorage.getItem('umbra-admin-sidebar') === 'collapsed';
    var sidebarMobileOpen = false;

    function isMobileSidebar() {
      return window.matchMedia && window.matchMedia('(max-width: 767px)').matches;
    }

    function applySidebarState() {
      var body = document.body;
      if (!body) return;
      var mobile = isMobileSidebar();
      body.classList.toggle('sidebar-collapsed', !mobile && sidebarCollapsed);
      body.classList.toggle('sidebar-open', mobile && sidebarMobileOpen);

      var toggle = document.getElementById('sidebar-toggle');
      if (toggle) {
        toggle.setAttribute('aria-expanded', mobile ? String(sidebarMobileOpen) : String(!sidebarCollapsed));
        toggle.setAttribute('aria-label', mobile
          ? (sidebarMobileOpen ? 'Close navigation' : 'Open navigation')
          : (sidebarCollapsed ? 'Expand navigation' : 'Collapse navigation'));
      }
      if (mobile || !sidebarCollapsed) hideSidebarTooltip();
    }

    function persistSidebarCollapsed() {
      fetch(umbraAdminBase + '/api/prefs', {
        method: 'PUT',
        headers: Object.assign({ 'Content-Type': 'application/json' }, csrfHeaders()),
        body: JSON.stringify({ sidebar_collapsed: sidebarCollapsed })
      }).catch(function() { /* localStorage is enough for instant UX */ });
    }

    function toggleSidebar() {
      if (isMobileSidebar()) {
        sidebarMobileOpen = !sidebarMobileOpen;
      } else {
        sidebarCollapsed = !sidebarCollapsed;
        localStorage.setItem('umbra-admin-sidebar', sidebarCollapsed ? 'collapsed' : 'expanded');
        persistSidebarCollapsed();
      }
      applySidebarState();
      queueChartRefresh();
    }

    function closeSidebar() {
      if (isMobileSidebar()) {
        sidebarMobileOpen = false;
      } else {
        sidebarCollapsed = true;
        localStorage.setItem('umbra-admin-sidebar', 'collapsed');
        persistSidebarCollapsed();
      }
      applySidebarState();
      queueChartRefresh();
    }

    applySidebarState();
    window.addEventListener('resize', applySidebarState);

    function hideSidebarTooltip() {
      var tooltip = document.getElementById('umbra-sidebar-tooltip');
      if (!tooltip) return;
      tooltip.removeAttribute('data-open');
      tooltip.setAttribute('aria-hidden', 'true');
      tooltip.textContent = '';
    }

    function showSidebarTooltip(trigger) {
      if (isMobileSidebar() || !document.body.classList.contains('sidebar-collapsed')) return;
      var text = trigger.getAttribute('data-sidebar-tooltip') || '';
      var tooltip = document.getElementById('umbra-sidebar-tooltip');
      if (!text || !tooltip) return;
      tooltip.textContent = text;
      tooltip.setAttribute('data-open', 'true');
      tooltip.setAttribute('aria-hidden', 'false');
      var rect = trigger.getBoundingClientRect();
      var y = rect.top + rect.height / 2;
      tooltip.style.transform = 'translateY(-50%)';
      tooltip.style.top = Math.max(18, Math.min(window.innerHeight - 18, y)) + 'px';
    }

    function initSidebarTooltips(root) {
      root = root || document;
      root.querySelectorAll('[data-sidebar-tooltip]:not([data-sidebar-tooltip-init])').forEach(function(el) {
        el.setAttribute('data-sidebar-tooltip-init', '1');
        el.addEventListener('mouseenter', function() { showSidebarTooltip(el); });
        el.addEventListener('focus', function() { showSidebarTooltip(el); });
        el.addEventListener('mouseleave', hideSidebarTooltip);
        el.addEventListener('blur', hideSidebarTooltip);
      });
    }
    initSidebarTooltips();

    // ----- Sidebar live model filter -----
    function sidebarFilter(query) {
      var q = query.trim().toLowerCase();
      var links = document.querySelectorAll('.sidebar-model-link');
      links.forEach(function(link) {
        var name = (link.getAttribute('data-model-name') || '').toLowerCase();
        var visible = q === '' || name.indexOf(q) !== -1;
        link.style.display = visible ? '' : 'none';
      });
      // Hide groups that have no visible models (except the core group).
      document.querySelectorAll('.sidebar-plugin-group').forEach(function(group) {
        var anyVisible = Array.from(group.querySelectorAll('.sidebar-model-link'))
          .some(function(l) { return l.style.display !== 'none'; });
        group.style.display = anyVisible ? '' : 'none';
      });
    }

    // ----- Dashboard charts -----
    function token(name, fallback) {
      var styles = getComputedStyle(document.documentElement);
      var value = styles.getPropertyValue(name);
      return value ? value.trim() : fallback;
    }

    function chartIsDark() {
      return document.documentElement.getAttribute('data-theme') !== 'light';
    }

    function readBarChart(el) {
      var labels = [];
      var values = [];
      el.querySelectorAll('[data-chart-point]').forEach(function(point) {
        var label = point.getAttribute('data-label') || '';
        var value = Number(point.getAttribute('data-value') || 0);
        labels.push(label);
        values.push(Number.isFinite(value) ? value : 0);
      });
      return {
        labels: labels,
        values: values,
        seriesName: el.getAttribute('data-chart-series') || 'entries',
      };
    }

    function barChartOptions(el) {
      var data = readBarChart(el);
      if (data.labels.length === 0) return null;
      var maxValue = Math.max.apply(null, data.values.concat([1]));
      var foreground = token('--on-surface', 'rgb(229 231 235)');
      var onPrimary = token('--on-primary', 'rgb(17 24 39)');
      var muted = token('--outline', 'rgb(148 163 184)');
      var grid = token('--outline-variant', 'rgb(51 65 85)');
      return {
        chart: {
          type: 'bar',
          height: '100%',
          width: '100%',
          background: 'transparent',
          fontFamily: 'Inter, ui-sans-serif, system-ui, sans-serif',
          foreColor: muted,
          parentHeightOffset: 0,
          toolbar: { show: false },
          zoom: { enabled: false },
          animations: {
            enabled: true,
            speed: 320,
            animateGradually: { enabled: false },
            dynamicAnimation: { enabled: true, speed: 220 },
          },
        },
        series: [{
          name: data.seriesName,
          data: data.values,
        }],
        colors: [token('--primary', 'rgb(79 70 229)')],
        plotOptions: {
          bar: {
            horizontal: true,
            borderRadius: 6,
            borderRadiusApplication: 'end',
            barHeight: '58%',
          },
        },
        dataLabels: {
          enabled: true,
          formatter: function(value) { return Math.round(value); },
          style: {
            colors: [onPrimary],
            fontFamily: 'Inter, ui-sans-serif, system-ui, sans-serif',
            fontSize: '11px',
            fontWeight: 600,
          },
          background: {
            enabled: false,
          },
        },
        grid: {
          borderColor: grid,
          strokeDashArray: 3,
          padding: { top: 0, right: 8, bottom: 0, left: 4 },
        },
        xaxis: {
          categories: data.labels,
          min: 0,
          max: maxValue,
          tickAmount: Math.min(4, Math.max(1, maxValue)),
          labels: {
            formatter: function(value) { return Math.round(value); },
            style: {
              colors: muted,
              fontFamily: 'Inter, ui-sans-serif, system-ui, sans-serif',
              fontSize: '11px',
            },
          },
          axisBorder: { show: false },
          axisTicks: { show: false },
        },
        yaxis: {
          labels: {
            align: 'left',
            minWidth: 0,
            maxWidth: 124,
            style: {
              colors: foreground,
              fontFamily: 'Inter, ui-sans-serif, system-ui, sans-serif',
              fontSize: '12px',
              fontWeight: 500,
            },
          },
        },
        tooltip: {
          theme: chartIsDark() ? 'dark' : 'light',
          y: {
            formatter: function(value) {
              var count = Math.round(value);
              return count + (count === 1 ? ' model' : ' models');
            },
          },
        },
        states: {
          hover: { filter: { type: 'lighten', value: 0.04 } },
          active: { filter: { type: 'none' } },
        },
      };
    }

    // Sparkline mode — area chart with chrome stripped. Used by
    // the card widget's trend trail; ApexCharts gives us
    // animation + tooltip + responsive resizing without any of
    // our own SVG plumbing. The colour comes from the card's
    // delta tone (emerald = positive, red = negative, default
    // primary) and is read off `data-spark-color`.
    function sparklineChartOptions(el) {
      var values = [];
      el.querySelectorAll('[data-chart-point]').forEach(function(point) {
        var v = Number(point.getAttribute('data-value') || 0);
        values.push(Number.isFinite(v) ? v : 0);
      });
      if (values.length === 0) return null;
      var color = el.getAttribute('data-spark-color') || token('--primary', 'rgb(99 102 241)');
      return {
        chart: {
          type: 'area',
          // ApexCharts sparkline mode needs an explicit pixel height —
          // `'100%'` evaluates to 0 inside an absolutely-positioned
          // canvas because the parent's computed height hasn't
          // finished layout when ApexCharts measures. 48px matches
          // the card's reserved sparkline strip (h-12 below).
          height: 48,
          width: '100%',
          background: 'transparent',
          sparkline: { enabled: true },
          animations: {
            enabled: true,
            speed: 380,
            animateGradually: { enabled: false },
            dynamicAnimation: { enabled: true, speed: 220 },
          },
        },
        series: [{ name: el.getAttribute('data-chart-series') || 'series', data: values }],
        colors: [color],
        stroke: { curve: 'smooth', width: 2 },
        fill: {
          type: 'gradient',
          gradient: {
            shadeIntensity: 1,
            opacityFrom: 0.35,
            opacityTo: 0,
            stops: [0, 100],
          },
        },
        tooltip: {
          enabled: true,
          theme: chartIsDark() ? 'dark' : 'light',
          fixed: { enabled: false },
          x: { show: false },
          y: { formatter: function(v) { return Math.round(v); } },
          marker: { show: false },
        },
      };
    }

    // Full-size line/area chart for dashboard widgets — like the
    // sparkline but with axes, grid, and tooltip x-labels.
    // Reads (x, y) pairs from `[data-chart-point][data-series][data-x][data-y]`
    // siblings; ApexCharts mounts on `[data-chart-canvas]`. Points
    // without `data-series` default to a single unnamed series, so
    // single-series widgets stay backwards compatible.
    function lineChartOptions(el) {
      var labels = [];
      var labelsSeen = Object.create(null);
      // Preserve insertion order of series so colors/legend match
      // the order the macro emitted them.
      var seriesOrder = [];
      var seriesMap = Object.create(null);
      el.querySelectorAll('[data-chart-point]').forEach(function(point) {
        var name = point.getAttribute('data-series') || 'series';
        var x = point.getAttribute('data-x') || '';
        var y = Number(point.getAttribute('data-y') || 0);
        var yClean = Number.isFinite(y) ? y : 0;
        if (!seriesMap[name]) {
          seriesMap[name] = [];
          seriesOrder.push(name);
        }
        seriesMap[name].push(yClean);
        // X labels — first series's x values define the axis;
        // subsequent series share them in order.
        if (!labelsSeen[x]) {
          labelsSeen[x] = true;
          labels.push(x);
        }
      });
      if (seriesOrder.length === 0) return null;
      var series = seriesOrder.map(function(name) {
        return { name: name, data: seriesMap[name] };
      });
      var primary = token('--primary', 'rgb(99 102 241)');
      var muted   = token('--outline', 'rgb(148 163 184)');
      var grid    = token('--outline-variant', 'rgb(51 65 85)');
      // Default palette for multi-series — emerald / blue / amber /
      // pink. Theme-stable accents (the framework only has
      // `--primary` semantically; everything else uses Tailwind
      // colors directly so it stays readable in light + dark).
      var palette = [primary, '#34d399', '#60a5fa', '#fbbf24', '#f472b6'];
      var isSingleSeries = series.length === 1;
      return {
        chart: {
          type: 'area',
          height: '100%',
          width: '100%',
          background: 'transparent',
          toolbar: { show: false },
          zoom: { enabled: false },
          parentHeightOffset: 0,
          fontFamily: 'Inter, ui-sans-serif, system-ui, sans-serif',
          foreColor: muted,
          animations: {
            enabled: true,
            speed: 380,
            animateGradually: { enabled: false },
            dynamicAnimation: { enabled: true, speed: 220 },
          },
        },
        series: series,
        colors: palette,
        // Single-series stays as the gradient area we had before;
        // multi-series drops the heavy fill so the lines don't
        // overlap into mud (ApexCharts area mode with multi-series
        // stacks fills by default — not what dashboard widgets
        // want; we want overlaid trends).
        stroke: { curve: 'smooth', width: 2 },
        fill: isSingleSeries
          ? {
              type: 'gradient',
              gradient: {
                shadeIntensity: 1,
                opacityFrom: 0.30,
                opacityTo: 0,
                stops: [0, 100],
              },
            }
          : { type: 'solid', opacity: 0.06 },
        dataLabels: { enabled: false },
        // Multi-series gets a legend strip; single-series doesn't
        // need it (the widget title IS the series name).
        legend: {
          show: !isSingleSeries,
          position: 'top',
          horizontalAlign: 'right',
          labels: { colors: muted },
          markers: { width: 8, height: 8, radius: 4 },
          itemMargin: { horizontal: 8 },
        },
        grid: {
          borderColor: grid,
          strokeDashArray: 3,
          padding: { top: 0, right: 8, bottom: 0, left: 4 },
        },
        markers: { size: 0, hover: { size: 4 } },
        xaxis: {
          categories: labels,
          labels: {
            style: { colors: muted, fontSize: '11px' },
            // Show every Nth label for dense series so they don't
            // crowd the axis. 7 ticks reads cleanly across widths.
            rotate: 0,
            hideOverlappingLabels: true,
          },
          axisBorder: { show: false },
          axisTicks: { show: false },
        },
        yaxis: {
          labels: {
            style: { colors: muted, fontSize: '11px' },
            formatter: function(v) { return Math.round(v); },
          },
        },
        tooltip: {
          theme: chartIsDark() ? 'dark' : 'light',
          y: { formatter: function(v) { return Math.round(v); } },
        },
      };
    }

    // Donut chart — labeled slices summing to 100%. Reads
    // (label, value, optional color) from
    // `[data-chart-slice][data-label][data-value][data-color]`
    // siblings. Legend renders on the right; center label
    // shows the total. Best for ≤6 slices — past that the
    // labels collide and a bar chart reads better.
    function donutChartOptions(el) {
      var labels = [];
      var values = [];
      var explicitColors = [];
      var hasAnyColor = false;
      el.querySelectorAll('[data-chart-slice]').forEach(function(slice) {
        labels.push(slice.getAttribute('data-label') || '');
        var v = Number(slice.getAttribute('data-value') || 0);
        values.push(Number.isFinite(v) ? v : 0);
        var c = slice.getAttribute('data-color');
        explicitColors.push(c || null);
        if (c) hasAnyColor = true;
      });
      if (values.length === 0) return null;
      var muted     = token('--outline', 'rgb(148 163 184)');
      var onSurface = token('--on-surface', 'rgb(229 231 235)');
      // Default palette mirrors the line chart — emerald / blue
      // / amber / pink / violet / cyan — readable in both
      // themes since they're explicit accents, not tokens.
      var defaultPalette = ['#34d399', '#60a5fa', '#fbbf24', '#f472b6', '#a78bfa', '#22d3ee'];
      var colors = hasAnyColor
        ? explicitColors.map(function(c, i) { return c || defaultPalette[i % defaultPalette.length]; })
        : defaultPalette;
      return {
        chart: {
          type: 'donut',
          height: '100%',
          width: '100%',
          background: 'transparent',
          fontFamily: 'Inter, ui-sans-serif, system-ui, sans-serif',
          foreColor: muted,
          parentHeightOffset: 0,
          animations: {
            enabled: true,
            speed: 380,
            animateGradually: { enabled: false },
            dynamicAnimation: { enabled: true, speed: 220 },
          },
        },
        series: values,
        labels: labels,
        colors: colors,
        stroke: { width: 0 },
        dataLabels: { enabled: false },
        legend: {
          position: 'right',
          labels: { colors: muted },
          markers: { width: 8, height: 8, radius: 4 },
          itemMargin: { vertical: 4 },
        },
        plotOptions: {
          pie: {
            donut: {
              size: '70%',
              labels: {
                show: true,
                name: {
                  show: true,
                  fontSize: '11px',
                  color: muted,
                  offsetY: -4,
                },
                value: {
                  show: true,
                  fontSize: '20px',
                  fontWeight: 600,
                  color: onSurface,
                  offsetY: 6,
                  formatter: function(v) { return Math.round(Number(v)); },
                },
                total: {
                  show: true,
                  label: 'Total',
                  color: muted,
                  fontSize: '11px',
                  formatter: function(w) {
                    return Math.round(
                      w.globals.seriesTotals.reduce(function(a, b) { return a + b; }, 0)
                    );
                  },
                },
              },
            },
          },
        },
        tooltip: {
          theme: chartIsDark() ? 'dark' : 'light',
          y: { formatter: function(v) { return Math.round(v); } },
        },
        responsive: [{
          breakpoint: 480,
          options: {
            legend: { position: 'bottom' },
          },
        }],
      };
    }

    function initCharts(root) {
      root = root || document;
      root.querySelectorAll('[data-umbra-chart="bar"]').forEach(function(el) {
        var canvas = el.querySelector('[data-chart-canvas]');
        var fallback = el.querySelector('[data-chart-unavailable]');
        if (!canvas) return;
        if (!window.ApexCharts) {
          if (fallback) fallback.classList.remove('hidden');
          return;
        }
        if (fallback) fallback.classList.add('hidden');
        var options = barChartOptions(el);
        if (!options) return;
        if (canvas._umbraApexChart) {
          canvas._umbraApexChart.updateOptions(options, false, true);
          return;
        }
        canvas._umbraApexChart = new ApexCharts(canvas, options);
        canvas._umbraApexChart.render();
      });
      root.querySelectorAll('[data-umbra-chart="sparkline"]').forEach(function(el) {
        var canvas = el.querySelector('[data-chart-canvas]');
        var fallback = el.querySelector('[data-chart-unavailable]');
        if (!canvas) return;
        if (!window.ApexCharts) {
          if (fallback) fallback.classList.remove('hidden');
          return;
        }
        if (fallback) fallback.classList.add('hidden');
        var options = sparklineChartOptions(el);
        if (!options) return;
        if (canvas._umbraApexChart) {
          canvas._umbraApexChart.updateOptions(options, false, true);
          return;
        }
        canvas._umbraApexChart = new ApexCharts(canvas, options);
        canvas._umbraApexChart.render();
      });
      root.querySelectorAll('[data-umbra-chart="line"]').forEach(function(el) {
        var canvas = el.querySelector('[data-chart-canvas]');
        var fallback = el.querySelector('[data-chart-unavailable]');
        if (!canvas) return;
        if (!window.ApexCharts) {
          if (fallback) fallback.classList.remove('hidden');
          return;
        }
        if (fallback) fallback.classList.add('hidden');
        var options = lineChartOptions(el);
        if (!options) return;
        if (canvas._umbraApexChart) {
          canvas._umbraApexChart.updateOptions(options, false, true);
          return;
        }
        canvas._umbraApexChart = new ApexCharts(canvas, options);
        canvas._umbraApexChart.render();
      });
      root.querySelectorAll('[data-umbra-chart="donut"]').forEach(function(el) {
        var canvas = el.querySelector('[data-chart-canvas]');
        var fallback = el.querySelector('[data-chart-unavailable]');
        if (!canvas) return;
        if (!window.ApexCharts) {
          if (fallback) fallback.classList.remove('hidden');
          return;
        }
        if (fallback) fallback.classList.add('hidden');
        var options = donutChartOptions(el);
        if (!options) return;
        if (canvas._umbraApexChart) {
          canvas._umbraApexChart.updateOptions(options, false, true);
          return;
        }
        canvas._umbraApexChart = new ApexCharts(canvas, options);
        canvas._umbraApexChart.render();
      });
    }

    function queueChartRefresh() {
      window.setTimeout(function() {
        initCharts(document);
      }, 220);
    }

    // ----- User menu (placeholder) -----
    function toggleUserMenu() {
      // Phase 4 will wire a real dropdown; for now clicking the avatar
      // navigates to ${admin_base}/logout for simplicity.
    }

    Object.assign(window.umbra, {
      toggleTheme: toggleTheme,
      toggleSidebar: toggleSidebar,
      closeSidebar: closeSidebar,
      sidebarFilter: sidebarFilter,
      initCharts: initCharts,
      refreshCharts: queueChartRefresh,
      toggleUserMenu: toggleUserMenu,
    });
  })();

  // Initialise Lucide icons after the DOM is fully painted and after
  // HTMX swaps widget/palette fragments into the page.
  document.addEventListener('DOMContentLoaded', function() {
    if (window.lucide) lucide.createIcons();
    if (window.umbra && umbra.initCharts) umbra.initCharts(document);
  });
  document.body.addEventListener('htmx:afterSwap', function(e) {
    if (window.lucide) lucide.createIcons({ el: e.target });
    if (window.umbra && umbra.initCharts) umbra.initCharts(e.target);
  });
(function() {
  window.umbra = window.umbra || {};

  umbra.showToast = function(message, level) {
    level = level || 'info';
    var container = document.getElementById('umbra-toast-container');
    if (!container) return;
    var colors = {
      info:    'bg-surface-container border-outline-variant text-on-surface',
      success: 'bg-primary-container/20 border-primary/30 text-primary',
      warning: 'bg-surface-container border-outline text-on-surface-variant',
      error:   'bg-error-container/20 border-error/30 text-error'
    };
    var icons = { info: 'info', success: 'check-circle', warning: 'alert-triangle', error: 'alert-circle' };
    var toast = document.createElement('div');
    toast.className = 'pointer-events-auto flex items-center gap-sm px-lg py-sm rounded-xl border shadow-lg font-label-md text-label-md transition-all duration-300 ' + (colors[level] || colors.info);
    toast.innerHTML = '<i data-lucide="' + (icons[level]||'info') + '" class="w-4 h-4 flex-shrink-0"></i><span>' + message + '</span>';
    container.appendChild(toast);
    if (window.lucide) lucide.createIcons({ el: toast });
    setTimeout(function() {
      toast.style.opacity = '0';
      toast.style.transform = 'translateX(20px)';
      setTimeout(function() { toast.remove(); }, 300);
    }, 4000);
  };

  document.body.addEventListener('htmx:responseError', function(e) {
    // HTMX classifies every 4xx + 5xx as a "response error" — but
    // 400 / 409 / 422 from the admin's form-submit handlers are
    // validation responses that re-render the form with an inline
    // error span ALREADY visible to the user. Firing a generic
    // "Server error" toast on top of that is noise: the user sees
    // both the precise field-level message AND a misleading
    // "server" toast that suggests a crash. Skip the toast for
    // these statuses; swap-bearing validation responses already
    // surface the real message inline. Only 5xx + the catch-all
    // 4xxs that DON'T carry a re-render body get the toast.
    var status = e.detail && e.detail.xhr ? e.detail.xhr.status : 0;
    if (status === 400 || status === 409 || status === 422) {
      return;
    }
    umbra.showToast('Server error', 'error');
  });

  document.body.addEventListener('showToast', function(e) {
    if (e.detail) umbra.showToast(e.detail.message, e.detail.level);
  });

  // MultiChoice — chip checkboxes synced to a hidden CSV input.
  // Idempotent via [data-mc-init]; safe to re-run after HTMX swaps.
  function initMultiChoicePickers(root) {
    root = root || document;
    root.querySelectorAll('.multichoice-picker:not([data-mc-init])').forEach(function(picker) {
      picker.setAttribute('data-mc-init', '1');
      var hidden = picker.querySelector('input[type=hidden]');
      if (!hidden) return;
      function sync() {
        var values = [];
        picker.querySelectorAll('input[type=checkbox][data-mc-value]').forEach(function(cb) {
          if (cb.checked) values.push(cb.getAttribute('data-mc-value'));
        });
        hidden.value = values.join(',');
        picker.querySelectorAll('input[type=checkbox][data-mc-value]').forEach(function(cb) {
          var label = cb.closest('label');
          if (!label) return;
          if (cb.checked) {
            label.classList.add('border-primary', 'text-primary', 'bg-primary/5');
            label.classList.remove('border-outline-variant', 'text-on-surface-variant');
          } else {
            label.classList.remove('border-primary', 'text-primary', 'bg-primary/5');
            label.classList.add('border-outline-variant', 'text-on-surface-variant');
          }
        });
      }
      picker.querySelectorAll('input[type=checkbox][data-mc-value]').forEach(function(cb) {
        cb.addEventListener('change', sync);
      });
    });
  }
  umbra.initMultiChoicePickers = initMultiChoicePickers;
  initMultiChoicePickers();
  document.body.addEventListener('htmx:afterSwap', function() { initMultiChoicePickers(); });

  // FK searchable combobox — idempotent via [data-fk-init].
  function initFkPickers(root) {
    root = root || document;
    root.querySelectorAll('.fk-picker:not([data-fk-init])').forEach(function(picker) {
      picker.setAttribute('data-fk-init', '1');
      var textInput = picker.querySelector('input[type=text]');
      var hiddenInput = picker.querySelector('input[type=hidden]');
      var dropdown = picker.querySelector('.fk-options');
      if (!textInput || !hiddenInput || !dropdown) return;

      function setSelection(value, label) {
        hiddenInput.value = value || '';
        var active = picker.querySelector('[data-fk-active]');
        var activeLabel = picker.querySelector('[data-fk-active-label]');
        var activeValue = picker.querySelector('[data-fk-active-value]');
        if (active) active.classList.toggle('hidden', !value);
        if (activeLabel) activeLabel.textContent = label || 'Selected option';
        if (activeValue) activeValue.textContent = value ? '#' + value : '';
      }

      textInput.addEventListener('focus', function() { dropdown.classList.remove('hidden'); });
      textInput.addEventListener('input', function() {
        if (textInput.value.trim()) dropdown.classList.remove('hidden');
      });
      document.addEventListener('click', function(e) {
        if (!picker.contains(e.target)) dropdown.classList.add('hidden');
      });

      picker.querySelectorAll('[data-fk-clear]').forEach(function(btn) {
        btn.addEventListener('click', function() {
          setSelection('', '');
          textInput.value = '';
          textInput.focus();
        });
      });

      picker.addEventListener('htmx:afterSwap', function() {
        dropdown.classList.remove('hidden');
        dropdown.querySelectorAll('[data-fk-value]').forEach(function(opt) {
          opt.addEventListener('mousedown', function(e) {
            e.preventDefault();
            var value = opt.getAttribute('data-fk-value') || '';
            var label = opt.getAttribute('data-fk-label') || opt.textContent.trim();
            setSelection(value, label);
            textInput.value = '';
            dropdown.classList.add('hidden');
          });
        });
        if (window.lucide) lucide.createIcons({ el: dropdown });
      });
    });
  }
  umbra.fkResolve = function(field, event) {
    try {
      var data = JSON.parse(event.detail.xhr.responseText);
      if (data.items && data.items[0]) {
        var source = event.target || (event.detail && event.detail.elt);
        var picker = source && source.closest ? source.closest('.fk-picker') : null;
        if (!picker) {
          var el = document.getElementById('fk_text_' + field);
          picker = el && el.closest ? el.closest('.fk-picker') : null;
        }
        if (picker) {
          var item = data.items[0];
          var hidden = picker.querySelector('input[type=hidden]');
          var active = picker.querySelector('[data-fk-active]');
          var activeLabel = picker.querySelector('[data-fk-active-label]');
          var activeValue = picker.querySelector('[data-fk-active-value]');
          if (hidden) hidden.value = String(item.value);
          if (active) active.classList.remove('hidden');
          if (activeLabel) activeLabel.textContent = item.label;
          if (activeValue) activeValue.textContent = '#' + item.value;
        }
      }
    } catch(e) {}
  };
  initFkPickers();
  document.body.addEventListener('htmx:afterSwap', function() { initFkPickers(); });

  // M2M checkbox lists — search + selected summary + small client-side pages.
  function initM2MPickers(root) {
    root = root || document;
    root.querySelectorAll('.m2m-field-picker:not([data-m2m-init])').forEach(function(picker) {
      picker.setAttribute('data-m2m-init', '1');
      var search = picker.querySelector('[data-m2m-search]');
      var selected = picker.querySelector('[data-m2m-selected]');
      var selectedEmpty = picker.querySelector('[data-m2m-selected-empty]');
      var count = picker.querySelector('[data-m2m-count]');
      var pageLabel = picker.querySelector('[data-m2m-page]');
      var prev = picker.querySelector('[data-m2m-prev]');
      var next = picker.querySelector('[data-m2m-next]');
      var empty = picker.querySelector('[data-m2m-empty]');
      var options = Array.from(picker.querySelectorAll('[data-m2m-option]'));
      var page = 1;
      var pageSize = parseInt(picker.getAttribute('data-page-size') || '12', 10);

      function labelFor(option) {
        return (option.getAttribute('data-label') || option.textContent || '').trim();
      }

      function checkedOptions() {
        return options.filter(function(option) {
          var cb = option.querySelector('input[type=checkbox]');
          return cb && cb.checked;
        });
      }

      function renderSelected() {
        if (!selected) return;
        selected.innerHTML = '';
        var checked = checkedOptions();
        if (count) count.textContent = checked.length + ' selected';
        if (selectedEmpty) selectedEmpty.classList.toggle('hidden', checked.length > 0);
        checked.slice(0, 8).forEach(function(option) {
          var cb = option.querySelector('input[type=checkbox]');
          var chip = document.createElement('button');
          chip.type = 'button';
          chip.className = 'inline-flex items-center gap-xs rounded-full border border-primary/25 bg-primary-container px-sm py-xs text-label-sm text-on-primary-container';
          var label = document.createElement('span');
          label.className = 'max-w-[160px] truncate';
          label.textContent = labelFor(option);
          var remove = document.createElement('span');
          remove.setAttribute('aria-hidden', 'true');
          remove.textContent = '×';
          chip.appendChild(label);
          chip.appendChild(remove);
          chip.addEventListener('click', function() {
            cb.checked = false;
            render();
          });
          selected.appendChild(chip);
        });
        if (checked.length > 8) {
          var more = document.createElement('span');
          more.className = 'text-label-sm text-outline px-xs py-xs';
          more.textContent = '+' + (checked.length - 8) + ' more';
          selected.appendChild(more);
        }
      }

      function render() {
        var q = search ? search.value.trim().toLowerCase() : '';
        var matches = options.filter(function(option) {
          return !q || labelFor(option).toLowerCase().indexOf(q) !== -1;
        });
        var pages = Math.max(1, Math.ceil(matches.length / pageSize));
        if (page > pages) page = pages;
        var start = (page - 1) * pageSize;
        var visible = new Set(matches.slice(start, start + pageSize));
        options.forEach(function(option) {
          option.classList.toggle('hidden', !visible.has(option));
        });
        if (empty) empty.classList.toggle('hidden', matches.length > 0);
        if (pageLabel) pageLabel.textContent = 'Page ' + page + ' of ' + pages;
        if (prev) prev.disabled = page <= 1;
        if (next) next.disabled = page >= pages;
        renderSelected();
      }

      if (search) {
        search.addEventListener('input', function() {
          page = 1;
          render();
        });
      }
      if (prev) prev.addEventListener('click', function() { page = Math.max(1, page - 1); render(); });
      if (next) next.addEventListener('click', function() { page = page + 1; render(); });
      options.forEach(function(option) {
        var cb = option.querySelector('input[type=checkbox]');
        if (cb) cb.addEventListener('change', render);
      });
      render();
    });
  }
  umbra.initM2MPickers = initM2MPickers;
  initM2MPickers();
  document.body.addEventListener('htmx:afterSwap', function() { initM2MPickers(); });
})();
(function() {
  // ----- Rich field-editor widgets (features.md #4) -----
  // #[umbra(widget = "...")] renders a <textarea data-widget> in the
  // form (see _macros/field_editor.html). Here we progressively enhance
  // each into a real editor:
  //   - "markdown" -> EasyMDE (toolbar + live side-by-side preview).
  //                   Render the stored value with `{{ value | markdown }}`.
  //   - "rte"      -> Quill (snow theme); the editor edits a div, we
  //                   sync its HTML back into the hidden <textarea> so
  //                   the form posts it. Render with `{{ value | sanitize }}`.
  //   - "code"     -> CodeMirror (JSON syntax + line numbers) for JSON /
  //                   structured text on a String / Json column.
  // Previews/loads are SANDBOXED: anything rendered into EasyMDE's
  // preview pane, or loaded into Quill, passes through DOMPurify first,
  // so authored content can't execute script in the admin origin.
  // Libraries LAZY-load from CDN only when a matching textarea is on the
  // page, so list/dashboard pages stay light. With no JS (or a CDN
  // failure) every field degrades to a plain, usable textarea — the
  // stored value is markdown / HTML / JSON text either way.
  window.umbra = window.umbra || {};

  var MD_CSS = 'https://unpkg.com/easymde@2.18.0/dist/easymde.min.css';
  var MD_JS  = 'https://unpkg.com/easymde@2.18.0/dist/easymde.min.js';
  var RTE_CSS = 'https://cdn.jsdelivr.net/npm/quill@2.0.3/dist/quill.snow.css';
  var RTE_JS  = 'https://cdn.jsdelivr.net/npm/quill@2.0.3/dist/quill.js';
  // DOMPurify sandboxes the editor previews: EasyMDE renders markdown
  // with `marked` (no sanitize) and Quill loads existing content via
  // `dangerouslyPasteHTML` — both would otherwise let authored content
  // execute <script>/onerror in the admin's own origin (real risk when
  // an admin previews a moderation-queue submission). Loaded alongside
  // each editor; the server-side `| markdown` / `| sanitize` filters are
  // the matching display-side layer (defense in depth).
  var PURIFY_JS = 'https://cdn.jsdelivr.net/npm/dompurify@3.1.6/dist/purify.min.js';
  // CodeMirror powers the `code` widget (JSON + structured text):
  // highlighting + line numbers. The JSON `mode` script depends on the
  // core being loaded first (see the load sequence in initWidgetEditors).
  var CM_CSS  = 'https://cdn.jsdelivr.net/npm/codemirror@5.65.16/lib/codemirror.min.css';
  var CM_JS   = 'https://cdn.jsdelivr.net/npm/codemirror@5.65.16/lib/codemirror.min.js';
  var CM_MODE = 'https://cdn.jsdelivr.net/npm/codemirror@5.65.16/mode/javascript/javascript.min.js';

  // Sanitize HTML before it lands in a preview pane. Uses DOMPurify when
  // present; if it somehow isn't loaded yet, fail CLOSED (drop all tags)
  // rather than render untrusted HTML.
  function previewClean(html) {
    if (window.DOMPurify) return window.DOMPurify.sanitize(html);
    var d = document.createElement('div');
    d.textContent = html;
    return d.innerHTML;
  }

  // CDN loader — inject each asset once; resolve the script's promise
  // when it's ready so multiple textareas share one load.
  var assets = {};
  function loadCss(url) {
    if (assets[url]) return;
    assets[url] = true;
    var l = document.createElement('link');
    l.rel = 'stylesheet';
    l.href = url;
    document.head.appendChild(l);
  }
  function loadScript(url) {
    if (assets[url]) return assets[url];
    assets[url] = new Promise(function(resolve, reject) {
      var s = document.createElement('script');
      s.src = url;
      s.async = true;
      s.onload = function() { resolve(); };
      s.onerror = function() { reject(new Error('umbra: failed to load ' + url)); };
      document.head.appendChild(s);
    });
    return assets[url];
  }

  // Claim every not-yet-mounted textarea for `selector` synchronously
  // (mark before the async load) so overlapping scans can't double-mount.
  function claim(root, selector) {
    var out = [];
    var nodes = root.querySelectorAll('textarea[data-widget="' + selector + '"]:not([data-widget-mounted])');
    for (var i = 0; i < nodes.length; i++) {
      nodes[i].setAttribute('data-widget-mounted', '1');
      out.push(nodes[i]);
    }
    return out;
  }

  function mountMarkdown(ta) {
    var mde = new EasyMDE({
      element: ta,
      spellChecker: false,
      status: false,
      minHeight: '220px',
      autoDownloadFontAwesome: true,
      // Sandbox the live preview: every rendered-HTML chunk EasyMDE is
      // about to inject goes through DOMPurify first.
      renderingConfig: { sanitizerFunction: previewClean },
      toolbar: ['bold', 'italic', 'heading', '|', 'quote', 'unordered-list',
                'ordered-list', '|', 'link', 'code', 'table', '|',
                'preview', 'side-by-side', 'guide']
    });
    // EasyMDE keeps the underlying textarea in sync; force a final flush
    // on submit so the posted value can't lag the last keystroke.
    var form = ta.closest('form');
    if (form) form.addEventListener('submit', function() { mde.codemirror.save(); });
  }

  function mountRte(ta) {
    ta.style.display = 'none';
    var wrap = document.createElement('div');
    wrap.className = 'umbra-rte';
    var host = document.createElement('div');
    wrap.appendChild(host);
    ta.parentNode.insertBefore(wrap, ta.nextSibling);

    var quill = new Quill(host, {
      theme: 'snow',
      modules: {
        toolbar: [
          ['bold', 'italic', 'underline', 'strike'],
          ['blockquote', 'code-block'],
          [{ header: [1, 2, 3, false] }],
          [{ list: 'ordered' }, { list: 'bullet' }],
          ['link'],
          ['clean']
        ]
      }
    });
    // Sanitize the stored HTML before loading it into the editor —
    // `dangerouslyPasteHTML` would otherwise run any injected markup.
    if (ta.value) quill.clipboard.dangerouslyPasteHTML(previewClean(ta.value));
    function sync() {
      // Quill's "empty" sentinel is <p><br></p>; store '' so a blank
      // RTE doesn't trip a NOT-NULL / required check with junk markup.
      var html = quill.root.innerHTML;
      ta.value = (html === '<p><br></p>') ? '' : html;
    }
    quill.on('text-change', sync);
    var form = ta.closest('form');
    if (form) form.addEventListener('submit', sync);
  }

  function mountCode(ta) {
    var cm = CodeMirror.fromTextArea(ta, {
      mode: { name: 'javascript', json: true },
      lineNumbers: true,
      tabSize: 2,
      lineWrapping: true,
      viewportMargin: Infinity
    });
    cm.getWrapperElement().classList.add('umbra-code');
    // CodeMirror writes back to the textarea on save(); flush on submit.
    var form = ta.closest('form');
    if (form) form.addEventListener('submit', function() { cm.save(); });
  }

  function mountAll(list, fn, label) {
    list.forEach(function(ta) {
      try {
        fn(ta);
      } catch (e) {
        // Editor mount failed — un-hide the textarea so the field is
        // still editable, and log loudly. Never leave a dead input.
        ta.style.display = '';
        if (window.console) console.error('umbra: ' + label + ' editor mount failed', e);
      }
    });
  }

  function unhide(list) {
    list.forEach(function(t) { t.style.display = ''; });
  }

  function initWidgetEditors(root) {
    root = root || document;
    var mds = claim(root, 'markdown');
    var rtes = claim(root, 'rte');
    var codes = claim(root, 'code');
    if (mds.length) {
      loadCss(MD_CSS);
      // DOMPurify must be present before the preview renders.
      Promise.all([loadScript(PURIFY_JS), loadScript(MD_JS)])
        .then(function() { mountAll(mds, mountMarkdown, 'markdown'); })
        .catch(function(e) { unhide(mds); if (window.console) console.error(e); });
    }
    if (rtes.length) {
      loadCss(RTE_CSS);
      Promise.all([loadScript(PURIFY_JS), loadScript(RTE_JS)])
        .then(function() { mountAll(rtes, mountRte, 'rte'); })
        .catch(function(e) { unhide(rtes); if (window.console) console.error(e); });
    }
    if (codes.length) {
      loadCss(CM_CSS);
      // The JSON mode script extends the already-loaded core, so load
      // core first, then the mode, then mount.
      loadScript(CM_JS)
        .then(function() { return loadScript(CM_MODE); })
        .then(function() { mountAll(codes, mountCode, 'code'); })
        .catch(function(e) { unhide(codes); if (window.console) console.error(e); });
    }
  }

  umbra.initWidgetEditors = initWidgetEditors;
  document.addEventListener('DOMContentLoaded', function() { initWidgetEditors(document); });
  // Forms arrive via htmx (changelist actions, inline edit) and via the
  // sheet stack's innerHTML injection — cover both. The mounted-marker
  // makes re-scans idempotent.
  document.body.addEventListener('htmx:afterSwap', function(e) { initWidgetEditors(e.target); });
})();
(function() {
  // Sheet stack state machine.
  window.umbra = window.umbra || {};
  var stack = [];

  umbra.openSheet = function(html) {
    var slot = document.getElementById('umbra-sheet-slot');
    if (!slot) return;
    stack.push(slot.innerHTML);
    slot.innerHTML = html;
    document.body.classList.add('overflow-hidden');
    if (window.lucide) lucide.createIcons({ el: slot });
    // The sheet form is injected via innerHTML (not an htmx swap), so
    // mount the markdown / RTE editors on it explicitly.
    if (umbra.initWidgetEditors) umbra.initWidgetEditors(slot);
    umbra._applyStackOffsets();
  };

  umbra.popSheet = function() {
    var slot = document.getElementById('umbra-sheet-slot');
    if (!slot) return;
    if (stack.length > 0) {
      slot.innerHTML = stack.pop();
      if (window.lucide) lucide.createIcons({ el: slot });
      if (umbra.initWidgetEditors) umbra.initWidgetEditors(slot);
      umbra._applyStackOffsets();
    } else {
      umbra.closeSheet();
    }
  };

  umbra.closeSheet = function() {
    var slot = document.getElementById('umbra-sheet-slot');
    if (slot) slot.innerHTML = '';
    stack = [];
    document.body.classList.remove('overflow-hidden');
  };

  // HX-Trigger handlers — the update handler emits these after a
  // successful Save (closes the sheet + refreshes the table) so the
  // changelist updates without a full page nav.
  document.body.addEventListener('closeSheet', function() {
    umbra.closeSheet();
  });
  document.body.addEventListener('refreshTable', function() {
    // When we're on a changelist page (#table-body present), re-fetch
    // the rows fragment with the current URL state. The URL bar reflects
    // any active search / sort / filter / pagination because the
    // changelist controls push their state with `hx-push-url="true"`,
    // so reading window.location is the freshest signal — fresher than
    // the search input's `hx-get` attribute (which stays at its initial
    // render value) or a stashed data-* on the tbody.
    //
    // Off the changelist (detail page, dashboard, etc.) there's no
    // table to swap — fall back to a full reload so the page picks up
    // the new values.
    var tbody = document.getElementById('table-body');
    if (!tbody) {
      window.location.reload();
      return;
    }
    // Synthesize the rows URL by inserting `/rows` between the table
    // segment and the query string. The changelist URL is
    // `/admin/<table>/?<params>`; the rows fragment lives at
    // `/admin/<table>/rows?<params>`.
    var pathname = window.location.pathname.replace(/\/+$/, '');
    var rows_path = pathname + '/rows';
    var url = rows_path + window.location.search;
    htmx.ajax('GET', url, { target: '#table-body', swap: 'innerHTML' });
  });

  umbra._applyStackOffsets = function() {
    var slot = document.getElementById('umbra-sheet-slot');
    if (!slot) return;
    var panels = slot.querySelectorAll('#umbra-sheet-panel');
    panels.forEach(function(panel, i) {
      panel.style.transform = 'translateX(-' + (i * 40) + 'px)';
    });
  };

  umbra.openNestedSheet = function(table) {
    htmx.ajax('GET', umbraAdminBase + '/' + table + '/new-sheet', {
      handler: function(elt, info) {
        umbra.openSheet(info.xhr.responseText);
      }
    });
  };

  umbra.closeDialog = function() {
    var slot = document.getElementById('umbra-dialog-slot');
    if (slot) slot.innerHTML = '';
  };

  // ---- Change password dialog (gaps2 #3) ----
  //
  // Pre-fix this built ~25 lines of dialog HTML via string concat
  // inside JS. Designers couldn't edit the markup without touching
  // a script tag; Tailwind's content scanner couldn't find every
  // class used in the dialog without scanning the JS literally;
  // every Tailwind class lived twice (here AND in any template that
  // wanted the same styling). The dialog now lives as a
  // `<template id="umbra-change-password-dialog-template">` block
  // higher up in this file. The opener clones the template content
  // and patches only the form's `hx-post` URL — that's the only
  // call-site-varying piece.
  umbra._openChangePasswordDialog = function(table, id) {
    var slot = document.getElementById('umbra-dialog-slot');
    var tpl = document.getElementById('umbra-change-password-dialog-template');
    if (!slot || !tpl || !tpl.content) return;
    var node = tpl.content.cloneNode(true);
    var form = node.querySelector('[data-change-pw-form]');
    if (form) {
      form.setAttribute('hx-post', umbraAdminBase + '/' + table + '/' + id + '/change-password');
    }
    slot.innerHTML = '';
    slot.appendChild(node);
    if (window.lucide) lucide.createIcons({ el: slot });
    htmx.process(slot);
  };

  // Escape: close top sheet only.
  document.addEventListener('keydown', function(e) {
    if (e.key === 'Escape') {
      // Close palette first if open.
      var palSlot = document.getElementById('umbra-palette-slot');
      if (palSlot && palSlot.innerHTML.trim()) { umbra.closePalette && umbra.closePalette(); e.preventDefault(); return; }
      var slot = document.getElementById('umbra-sheet-slot');
      if (slot && slot.innerHTML.trim()) { umbra.popSheet(); e.preventDefault(); }
    }
    // ⌘K / Ctrl-K: open command palette.
    if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
      e.preventDefault();
      var palSlot2 = document.getElementById('umbra-palette-slot');
      if (palSlot2 && palSlot2.innerHTML.trim()) {
        umbra.closePalette && umbra.closePalette();
      } else {
        htmx.ajax('GET', umbraAdminBase + '/api/palette', {
          target: '#umbra-palette-slot',
          swap: 'innerHTML'
        });
      }
    }
  });
})();
