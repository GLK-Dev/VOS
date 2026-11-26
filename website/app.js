(function () {
  const SIM_DATA = {
    pack: {
      cmd: 'vos pack vos "compile_context_pack token budget" --budget 1200 --compact',
      out: `Context Pack \`vos\` | 669/1200 tokens (55.8%) | 8 item(s) packed (1 omitted)

## Workspace & Session Pulse [72t]
Git: branch \`main\` (v2.0.0 clean)
Last commit: feat(v2.0): add token-budgeted context compiler, calibration, and HUD

## Verified Fact [29t]
[Active] (crates/vos-store/src/packer.rs) Greedy value-per-token knapsack stays strictly <= token_budget

## Symbol \`compile_context_pack\` [248t]
// crates/vos-store/src/packer.rs:15..98 (pub fn compile_context_pack(...))`
    },
    preflight: {
      cmd: 'vos preflight vos crates/vos-store/src/guard.rs',
      out: `Preflight for vos (targets: crates/vos-store/src/guard.rs):
  Blast: 2 direct dependent(s), 26 transitive dependent(s)
    <- crates/vos-store/README.md
    <- crates/vos-store/src/lib.rs
  Affected tests: 3
    - [d2] crates/vos-indexer/tests/search_benchmark.rs
    - [d2] crates/vos-store/tests/lifecycle_edge_cases.rs
    - [d2] scripts/test_24_sessions.ps1
  Suggested commands:
    cargo test -p vos-indexer --test search_benchmark
    cargo test -p vos-store --test lifecycle_edge_cases`
    },
    guard: {
      cmd: 'vos guard vos --strict',
      out: `PASS: vos (3 rule(s) checked via vos.rules.toml, 0 violations)
  ✓ cli-no-sqlite                    (apps/vos-cli -> forbid rusqlite, store/batch.rs)
  ✓ core-stays-pure                  (crates/vos-core -> forbid vos_store, vos_indexer)
  ✓ store-independent-of-indexer-mcp (crates/vos-store -> forbid vos_indexer, vos_mcp)`
    },
    arch: {
      cmd: 'vos archaeology crates/vos-store/src/search.rs --project vos',
      out: `Archaeology: crates/vos-store/src/search.rs (12 commits sampled)
  Co-change partners:
    - apps/vos-cli/src/main.rs         (11 co-commits, coupling 0.92)
    - apps/vos-cli/tests/cli_e2e.rs    (8 co-commits,  coupling 0.67)
    - crates/vos-core/src/models.rs    (8 co-commits,  coupling 0.67)
    - crates/vos-core/src/lib.rs       (7 co-commits,  coupling 0.58)`
    },
    cal: {
      cmd: 'vos calibrate',
      out: `Recovery Calibration (global): 3 scored session(s) | avg_dur: 128.7s | avg_quality: 4.67/5.0
  False alarms: 100% | Missed difficulty: 0%
  Weights: inact=1.00x cov=0.65x risk=1.00x q=1.00x size=0.70x dirty=+1
  - Dampened coverage (0.65x) and size (0.70x) weights due to 100% Hard false-alarm rate.`
    },
    ui: {
      cmd: 'vos ui --once',
      out: `VOS CONSTELLATION HUD | 11 projects | 1480 objects | 23 dirty files | 0 stale facts
Calibration: 3 scored session(s), false_alarm=100%, missed=0%
--------------------------------------------------------------------------------------------
* backend-core   | Medium |  500 objs | dirty: 5 (main)      | stale:0 leases:0 | areas: sdk, runtime
* ml-inference   | Medium |   50 objs | dirty: 4 (master)    | stale:0 leases:0 | areas: src/compute
* vos            | Easy   |   73 objs | dirty: 0 (main)      | stale:0 leases:0 | areas: crates/vos-store`
    }
  };

  function getLang() {
    const saved = localStorage.getItem('vos_lang');
    return saved === 'ru' ? 'ru' : 'en';
  }

  function applyLang(lang) {
    localStorage.setItem('vos_lang', lang);
    document.documentElement.lang = lang;
    const dict = (window.VOS_I18N && window.VOS_I18N[lang]) || {};
    document.querySelectorAll('[data-i18n]').forEach((el) => {
      const k = el.getAttribute('data-i18n');
      if (dict[k]) el.textContent = dict[k];
    });
    document.querySelectorAll('[data-en][data-ru]').forEach((el) => {
      el.textContent = lang === 'ru' ? el.getAttribute('data-ru') : el.getAttribute('data-en');
    });
    document.querySelectorAll('.lang-toggle button').forEach((btn) => {
      btn.classList.toggle('active', btn.getAttribute('data-lang') === lang);
    });
    if (typeof window.onVosLangChange === 'function') {
      window.onVosLangChange(lang);
    }
  }

  function initLangToggle() {
    document.querySelectorAll('.lang-toggle button').forEach((btn) => {
      btn.addEventListener('click', () => applyLang(btn.getAttribute('data-lang')));
    });
    applyLang(getLang());
  }

  function initSimulator() {
    const tabs = document.querySelectorAll('.sim-tab');
    const cmdEl = document.getElementById('simCmd');
    const outEl = document.getElementById('simOut');
    if (!tabs.length || !cmdEl || !outEl) return;

    function select(key) {
      const entry = SIM_DATA[key] || SIM_DATA.pack;
      tabs.forEach((t) => t.classList.toggle('active', t.getAttribute('data-sim') === key));
      cmdEl.textContent = '$ ' + entry.cmd;
      outEl.textContent = entry.out;
    }

    tabs.forEach((t) => t.addEventListener('click', () => select(t.getAttribute('data-sim'))));
    select('pack');
  }

  window.copyText = function (btn, text) {
    if (!navigator.clipboard) return;
    navigator.clipboard.writeText(text).then(() => {
      const prev = btn.textContent;
      btn.textContent = getLang() === 'ru' ? 'Скопировано' : 'Copied!';
      setTimeout(() => { btn.textContent = prev; }, 1400);
    });
  };

  window.getVosLang = getLang;
  document.addEventListener('DOMContentLoaded', () => {
    initLangToggle();
    initSimulator();
  });
})();
