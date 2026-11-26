window.VOS_WIKI = [
  {
    id: 'getting-started',
    tag: 'QUICK START',
    en: {
      title: '1. Getting Started & Onboarding',
      summary: 'Install VOS 2.0, register any Git/Rust/Python/TS/C++ repository in one command, and connect MCP clients with zero background daemons.',
      sections: [
        {
          heading: 'Install & Initialize',
          body: 'VOS ships as a single Rust binary backed by embedded SQLite + FTS5 at ~/.vos/vos.db (override with --db or VOS_DB).',
          code: 'cargo install --git https://github.com/GLK-Dev/VOS vos-cli\nvos init /path/to/repo --name my-app\nvos watch my-app --interval-ms 1500'
        },
        {
          heading: 'Zero-Daemon MCP Configuration',
          body: 'Running `vos init` safely scaffolds `.mcp.json` and `.vscode/mcp.json` without overwriting existing entries. You can also wire `vos mcp` manually:',
          code: '{\n  "mcpServers": {\n    "vos": { "command": "vos", "args": ["mcp"] }\n  }\n}'
        }
      ]
    },
    ru: {
      title: '1. Быстрый старт и онбординг',
      summary: 'Установите VOS 2.0, проиндексируйте репозиторий одной командой и подключите MCP-клиентов без фоновых демонов.',
      sections: [
        {
          heading: 'Установка и инициализация',
          body: 'VOS поставляется как один Rust-бинарник с локальной БД SQLite + FTS5 в ~/.vos/vos.db (можно переопределить через --db или VOS_DB).',
          code: 'cargo install --git https://github.com/GLK-Dev/VOS vos-cli\nvos init /path/to/repo --name my-app\nvos watch my-app --interval-ms 1500'
        },
        {
          heading: 'Настройка MCP без демонов',
          body: 'Команда `vos init` аккуратно добавляет конфиг в `.mcp.json` и `.vscode/mcp.json`, не стирая существующие серверы. Ручной конфиг:',
          code: '{\n  "mcpServers": {\n    "vos": { "command": "vos", "args": ["mcp"] }\n  }\n}'
        }
      ]
    }
  },
  {
    id: 'context-compiler',
    tag: 'V2.0 CORE',
    en: {
      title: '2. Token-Budgeted Context Compiler',
      summary: 'Compile an optimal prompt bundle strictly bounded by `--budget` tokens using a deterministic value-per-token knapsack solver.',
      sections: [
        {
          heading: 'How `vos pack` Scores & Packs Context',
          bullets: [
            'Workspace & Session Pulse: Git branch, dirty files, latest handoff, and return points.',
            'Active Leases & Verified Facts: collision warnings plus SHA-256 anchored invariants.',
            'Exact AST Symbol Slices: queries `symbols_fts` and slices exact function/struct spans.',
            '1-Hop Graph & Co-Change Neighbors: attaches related files without dumping whole repos.'
          ],
          code: 'vos pack my-app "refactor search ranking" --budget 2000 --raw\nvos pack my-app "fix auth token" --budget 6000 --json'
        }
      ]
    },
    ru: {
      title: '2. Компилятор контекста с бюджетом токенов',
      summary: 'Собирает идеальный контекст строго внутри `--budget` токенов с помощью детерминированной оптимизации «польза на токен».',
      sections: [
        {
          heading: 'Как работает `vos pack`',
          bullets: [
            'Пульс сессии и Git: текущая ветка, dirty-файлы, последний handoff и точки возврата.',
            'Аренды и факты: предупреждения о занятых областях и SHA-256 инварианты.',
            'Точные срезы AST: поиск по `symbols_fts` и извлечение конкретных функций/структур.',
            '1-hop соседи: прямые графовые зависимости и исторические co-change партнёры.'
          ],
          code: 'vos pack my-app "refactor search ranking" --budget 2000 --raw\nvos pack my-app "fix auth token" --budget 6000 --json'
        }
      ]
    }
  },
  {
    id: 'blast-guardrails',
    tag: 'V1.5 SAFETY',
    en: {
      title: '3. Pre-Flight Blast Simulator & Guardrails',
      summary: 'Inspect dependents, historical coupling, leases, affected tests, and enforce declarative layer rules before making risky edits.',
      sections: [
        {
          heading: 'Pre-Edit Safety Check & Minimal Tests',
          body: 'Run `vos preflight` or `vos affected-tests` targeting specific files (or omit targets to automatically inspect dirty Git files).',
          code: 'vos preflight my-app crates/core/src/lib.rs --json --compact\nvos affected-tests my-app crates/core/src/lib.rs\nvos guard my-app --strict'
        },
        {
          heading: 'Declarative `vos.rules.toml` Format',
          body: 'Guardrails inspect both code graph edges (RustMod, RustUse, CodeImport) and source import statements with zero extra dependencies:',
          code: '[[rule]]\nname = "cli-no-sqlite"\nfrom = "apps/vos-cli"\nforbid_import = ["rusqlite"]\nforbid_to = ["crates/vos-store/src/batch.rs"]\nreason = "CLI must stay free of direct SQLite queries"'
        }
      ]
    },
    ru: {
      title: '3. Pre-Flight симулятор и Guardrails',
      summary: 'Проверяйте зависимых, co-change связи, активные аренды, затронутые тесты и архитектурные границы до начала правок.',
      sections: [
        {
          heading: 'Проверка последствий и минимальных тестов',
          body: 'Запустите `vos preflight` или `vos affected-tests` для нужных файлов (без аргументов берутся текущие dirty-файлы из Git).',
          code: 'vos preflight my-app crates/core/src/lib.rs --json --compact\nvos affected-tests my-app crates/core/src/lib.rs\nvos guard my-app --strict'
        },
        {
          heading: 'Декларативные правила `vos.rules.toml`',
          body: 'Проверяются кодовые рёбра графа (RustMod, RustUse, CodeImport) и реальные импорты в исходниках:',
          code: '[[rule]]\nname = "cli-no-sqlite"\nfrom = "apps/vos-cli"\nforbid_import = ["rusqlite"]\nforbid_to = ["crates/vos-store/src/batch.rs"]\nreason = "CLI не должен обращаться к SQLite напрямую"'
        }
      ]
    }
  },
  {
    id: 'cognitive-bus',
    tag: 'V1.4 MEMORY',
    en: {
      title: '4. Multi-Agent Cognitive Bus',
      summary: 'Coordinate multiple coding agents or human sessions via hash-anchored facts, advisory leases, handoffs, and cross-project recall.',
      sections: [
        {
          heading: 'Self-Invalidating Facts (`facts_fts`)',
          body: 'Every file-bound fact records the file SHA-256 content hash. When re-indexing detects a file change, the fact automatically transitions to `[Stale]` until re-verified.',
          code: 'vos fact add "Non-work assets receive -40 rank penalty" --project my-app --file src/search.rs\nvos fact list --project my-app\nvos fact verify <fact_id>'
        },
        {
          heading: 'Area Leases, Handoffs & Global Recall',
          code: 'vos claim my-app crates/style --agent worker-1 --ttl-mins 45 --note "tokens"\nvos handoff my-app --from worker-1 --to worker-2 --summary "Updated tokens" --next "Run cargo test"\nvos release my-app --agent worker-1\nvos recall "deadlock"'
        }
      ]
    },
    ru: {
      title: '4. Когнитивная шина и координация агентов',
      summary: 'Координируйте работу нескольких агентов и сессий через hash-факты, временные аренды областей, handoff-записи и кросс-проектный поиск.',
      sections: [
        {
          heading: 'Самопроверяемые факты (`facts_fts`)',
          body: 'Факт запоминает SHA-256 хеш целевого файла. Как только код меняется при переиндексации, факт автоматически получает статус `[Stale]` до перепроверки.',
          code: 'vos fact add "Non-work assets receive -40 rank penalty" --project my-app --file src/search.rs\nvos fact list --project my-app\nvos fact verify <fact_id>'
        },
        {
          heading: 'Аренды областей, Handoff и Recall',
          code: 'vos claim my-app crates/style --agent worker-1 --ttl-mins 45 --note "tokens"\nvos handoff my-app --from worker-1 --to worker-2 --summary "Updated tokens" --next "Run cargo test"\nvos release my-app --agent worker-1\nvos recall "deadlock"'
        }
      ]
    }
  },
  {
    id: 'archaeology-symbols',
    tag: 'V1.3 AST & GIT',
    en: {
      title: '5. Decision Archaeology & AST Slicing',
      summary: 'Combine Git commit forensics, historical co-change ratios, doc mentions, and language-aware symbol slicing in milliseconds.',
      sections: [
        {
          heading: 'Temporal Coupling & Forensics',
          body: '`vos archaeology` scans Git history touching a file, computes co-change partners that shift together even without static imports, and surfaces doc links + work sessions.',
          code: 'vos archaeology src/search.rs --project my-app --limit 10\nvos why src/search.rs --project my-app'
        },
        {
          heading: 'Zero-Daemon Symbol Index (`symbols_fts`)',
          body: 'Extracts functions, structs, enums, traits, impls, classes, and Markdown headings across Rust, Python, TS/JS, C/C++, and Markdown.',
          code: 'vos symbols --project my-app "ranking"\nvos slice src/search.rs --project my-app --symbol score_hit\nvos slice src/search.rs --project my-app --lines 40-120'
        }
      ]
    },
    ru: {
      title: '5. Археология решений и AST-срезы',
      summary: 'Объединяйте историю Git-коммитов, статистику co-change, ссылки из документации и точные срезы символов без LSP-демонов.',
      sections: [
        {
          heading: 'Историческая связанность и расследование',
          body: '`vos archaeology` анализирует коммиты по файлу, находит скрытых co-change партнёров и связывает их с упоминаниями в доках и прошлыми сессиями.',
          code: 'vos archaeology src/search.rs --project my-app --limit 10\nvos why src/search.rs --project my-app'
        },
        {
          heading: 'Индекс символов `symbols_fts`',
          body: 'Парсит функции, структуры, трейты, классы и заголовки Markdown в Rust, Python, TS/JS, C/C++.',
          code: 'vos symbols --project my-app "ranking"\nvos slice src/search.rs --project my-app --symbol score_hit\nvos slice src/search.rs --project my-app --lines 40-120'
        }
      ]
    }
  },

  {
    id: 'calibration-hud',
    tag: 'V2.0 TELEMETRY',
    en: {
      title: '6. Self-Calibrating Recovery & Constellation HUD',
      summary: 'Adaptive recovery difficulty weights calibrated from your own session history plus an on-demand single-binary localhost Constellation HUD.',
      sections: [
        {
          heading: 'Empirical Recovery Calibration (`vos calibrate`)',
          body: 'When 3+ scored sessions exist in `work_sessions`, `vos calibrate` measures your false-alarm rate (predicted Hard, recovered quickly with high quality) and missed-difficulty rate, dynamically adjusting coverage, size, risk, open-question, and dirty-Git multipliers.',
          code: 'vos session start my-app --note "Fix parser"\nvos session stop my-app --quality 5 --note "Done"\nvos calibrate\nvos continue my-app --experimental'
        },
        {
          heading: 'On-Demand Constellation HUD (`vos ui`)',
          body: 'Launches an interactive SVG Constellation map on `127.0.0.1:4173` (or prints an instant terminal matrix via `--once`) and shuts down completely when closed — zero lingering daemon.',
          code: 'vos ui --once\nvos ui --port 4173'
        }
      ]
    },
    ru: {
      title: '6. Самокалибровка возврата и Constellation HUD',
      summary: 'Эмпирическая настройка весов сложности возврата по истории ваших сессий плюс однобинарный локальный Constellation HUD по требованию.',
      sections: [
        {
          heading: 'Автоматическая калибровка (`vos calibrate`)',
          body: 'Когда накоплено 3+ оценённые сессии в `work_sessions`, `vos calibrate` считает False Alarms и Missed Difficulty, корректируя веса покрытия, размера проекта, рисков и dirty Git.',
          code: 'vos session start my-app --note "Fix parser"\nvos session stop my-app --quality 5 --note "Done"\nvos calibrate\nvos continue my-app --experimental'
        },
        {
          heading: 'Constellation HUD (`vos ui`)',
          body: 'Поднимает локальный SVG-дашборд на `127.0.0.1:4173` (или матрицу в терминал по `--once`) с зумом до AST-символов и полностью закрывается при выходе — без фонового демона.',
          code: 'vos ui --once\nvos ui --port 4173'
        }
      ]
    }
  },
  {
    id: 'mcp-reference',
    tag: '35 MCP TOOLS',
    en: {
      title: '7. Complete 35-Tool MCP & Compact JSON Reference',
      summary: 'Stateless newline-delimited JSON-RPC 2.0 server (`vos mcp`) exposing every VOS capability cleanly to AI coding agents.',
      sections: [
        {
          heading: 'Exposed MCP Tools (35)',
          bullets: [
            'Context & Compiler: `vos_init`, `vos_list_projects`, `vos_continue`, `vos_status`, `vos_pack`, `vos_constellation`, `vos_calibrate`',
            'Search, AST & Archaeology: `vos_search`, `vos_show`, `vos_why`, `vos_symbols`, `vos_slice`, `vos_archaeology`, `vos_related`, `vos_graph`, `vos_drift`',
            'Safety & Cognitive Bus: `vos_preflight`, `vos_affected_tests`, `vos_guard`, `vos_fact`, `vos_claim`, `vos_release`, `vos_handoff`, `vos_recall`',
            'Lifecycle & Analytics: `vos_index`, `vos_session_start`, `vos_session_stop`, `vos_coverage`, `vos_evolution`, `vos_changed_since`, `vos_snapshot`, `vos_tags`, `vos_tag`, `vos_untag`, `vos_doctor`'
          ],
          code: 'vos continue my-app --experimental --json --compact\nvos preflight my-app crates/core/src/lib.rs --json --compact'
        }
      ]
    },
    ru: {
      title: '7. Справочник 35 MCP-инструментов и Compact JSON',
      summary: 'Stateless JSON-RPC 2.0 сервер (`vos mcp`) по stdio, открывающий все возможности VOS для AI-агентов с минимальным расходом токенов.',
      sections: [
        {
          heading: 'Все 35 инструментов MCP',
          bullets: [
            'Контекст и компилятор: `vos_init`, `vos_list_projects`, `vos_continue`, `vos_status`, `vos_pack`, `vos_constellation`, `vos_calibrate`',
            'Поиск, AST и археология: `vos_search`, `vos_show`, `vos_why`, `vos_symbols`, `vos_slice`, `vos_archaeology`, `vos_related`, `vos_graph`, `vos_drift`',
            'Безопасность и шина агентов: `vos_preflight`, `vos_affected_tests`, `vos_guard`, `vos_fact`, `vos_claim`, `vos_release`, `vos_handoff`, `vos_recall`',
            'Жизненный цикл и аудит: `vos_index`, `vos_session_start`, `vos_session_stop`, `vos_coverage`, `vos_evolution`, `vos_changed_since`, `vos_snapshot`, `vos_tags`, `vos_tag`, `vos_untag`, `vos_doctor`'
          ],
          code: 'vos continue my-app --experimental --json --compact\nvos preflight my-app crates/core/src/lib.rs --json --compact'
        }
      ]
    }
  },
  {
    id: 'architecture',
    tag: 'ARCHITECTURE & GUIL-1.0',
    en: {
      title: '8. Crate Boundaries, Storage & GUIL-1.0',
      summary: 'Strict separation between pure models, SQLite/FTS5 storage, filesystem indexer, MCP server, and CLI interface — verified by `vos guard`.',
      sections: [
        {
          heading: 'Five Workspace Crates',
          bullets: [
            '`crates/vos-core`: shared domain models, compact JSON projections, path/hash utilities (0 store/indexer deps).',
            '`crates/vos-store`: SQLite migrations `0001..0007`, FTS5 (`objects_fts`, `chunks_fts`, `symbols_fts`, `facts_fts`), compiler, guard, calibration, HUD.',
            '`crates/vos-indexer`: idempotent walker, multi-language relation/symbol extractors, `vos init`, foreground `vos watch`.',
            '`crates/vos-mcp`: stateless stdio JSON-RPC 2.0 server.',
            '`apps/vos-cli`: thin CLI + single-binary `vos ui` server.'
          ],
          code: 'cargo clippy --workspace --all-targets -- -D warnings\ncargo test --workspace\nvos guard vos --strict'
        },
        {
          heading: 'License (GUIL-1.0)',
          body: 'VOS (Velocity Orientation System) is authored by Vitaly Golik (GLK-Dev) and licensed under GLK-Dev Universal Innovation License v1.0 (GUIL-1.0). Free for personal, academic, open-source, and small-team use; clear commercial terms for larger organizations.'
        }
      ]
    },
    ru: {
      title: '8. Архитектура крейтов и лицензия GUIL-1.0',
      summary: 'Строгое разделение слоёв между моделями, SQLite/FTS5-хранилищем, индексатором, MCP и CLI — проверяется командой `vos guard`.',
      sections: [
        {
          heading: 'Пять крейтов workspace',
          bullets: [
            '`crates/vos-core`: модели, компактные JSON-проекции, утилиты путей/хешей (без зависимости от БД).',
            '`crates/vos-store`: миграции `0001..0007`, FTS5 (`objects_fts`, `chunks_fts`, `symbols_fts`, `facts_fts`), компилятор, guard, калибровка, HUD.',
            '`crates/vos-indexer`: идемпотентный сканер, парсеры связей и AST-символов, `vos init`, `vos watch`.',
            '`crates/vos-mcp`: stateless stdio JSON-RPC 2.0 сервер.',
            '`apps/vos-cli`: тонкий CLI и встроенный сервер `vos ui`.'
          ],
          code: 'cargo clippy --workspace --all-targets -- -D warnings\ncargo test --workspace\nvos guard vos --strict'
        },
        {
          heading: 'Лицензия (GUIL-1.0)',
          body: 'Автор VOS — Виталий Голик (GLK-Dev). Проект распространяется под лицензией GUIL-1.0: бесплатно для личного, образовательного, open-source и малого командного использования.'
        }
      ]
    }
  }
];
