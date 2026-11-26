//! On-demand single-binary local Constellation HUD (`vos ui`, `v2.0`).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use anyhow::{Context, Result};
use serde_json::json;
use vos_store::Store;

/// Launches the on-demand local Constellation HUD HTTP server until interrupted.
pub fn run_constellation_server(store: &mut Store, port: u16, open_browser: bool) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("failed to bind Constellation HUD on 127.0.0.1:{port}"))?;
    let bound_port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{bound_port}/");
    println!("VOS Constellation HUD listening at {url} (press Ctrl+C to stop; zero daemon when closed)");

    if open_browser {
        let _ = open_url_in_browser(&url);
    }

    for stream_res in listener.incoming() {
        let Ok(stream) = stream_res else {
            continue;
        };
        if let Ok(should_quit) = handle_connection(store, stream)
            && should_quit
        {
            break;
        }
    }
    Ok(())
}

fn handle_connection(store: &mut Store, mut stream: TcpStream) -> Result<bool> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(false);
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");

    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
    }

    if method == "POST" && target == "/api/shutdown" {
        let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\nConnection: close\r\n\r\n{\"stopped\":true}";
        stream.write_all(resp.as_bytes())?;
        return Ok(true);
    }

    let (status, content_type, body) = dispatch_ui_request(store, method, target);
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    Ok(false)
}

/// Deterministic HTTP route dispatcher shared by the live server and unit tests.
pub fn dispatch_ui_request(
    store: &mut Store,
    method: &str,
    raw_target: &str,
) -> (u16, &'static str, String) {
    let (path, query_str) = raw_target.split_once('?').unwrap_or((raw_target, ""));
    let params = parse_query(query_str);

    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => (200, "text/html; charset=utf-8", HUD_HTML.to_string()),
        ("GET", "/api/health") => (200, "application/json", "{\"ok\":true}".to_string()),
        ("GET", "/api/portfolio") => match store.constellation_portfolio() {
            Ok(p) => json_ok(&p),
            Err(e) => json_err(500, &e.to_string()),
        },
        ("GET", "/api/project") => {
            let Some(name) = params.get("name").filter(|s| !s.is_empty()) else {
                return json_err(400, "missing query param `name`");
            };
            match build_project_zoom(store, name) {
                Ok(v) => json_ok(&v),
                Err(e) => json_err(404, &e.to_string()),
            }
        }
        ("GET", "/api/file") => {
            let Some(project) = params.get("project").filter(|s| !s.is_empty()) else {
                return json_err(400, "missing query param `project`");
            };
            let Some(target) = params.get("target").filter(|s| !s.is_empty()) else {
                return json_err(400, "missing query param `target`");
            };
            match build_file_zoom(store, project, target) {
                Ok(v) => json_ok(&v),
                Err(e) => json_err(404, &e.to_string()),
            }
        }
        ("GET", "/api/pack") => {
            let Some(project) = params.get("project").filter(|s| !s.is_empty()) else {
                return json_err(400, "missing query param `project`");
            };
            let goal = params
                .get("goal")
                .map(String::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("project overview");
            let budget = params
                .get("budget")
                .and_then(|b| b.parse::<usize>().ok())
                .unwrap_or(2000);
            match store.compile_context_pack(project, goal, budget) {
                Ok(pack) => json_ok(&pack),
                Err(e) => json_err(400, &e.to_string()),
            }
        }
        ("POST", "/api/session/start") => {
            let Some(project) = params.get("project").filter(|s| !s.is_empty()) else {
                return json_err(400, "missing query param `project`");
            };
            let note = params.get("note").map(String::as_str);
            let res = store
                .start_session(project, note)
                .map(|s| json!({ "session_id": s.id, "project": s.project_name, "difficulty": s.predicted_difficulty }));
            match res {
                Ok(v) => json_ok(&v),
                Err(e) => json_err(400, &e.to_string()),
            }
        }
        ("POST", "/api/session/stop") => {
            let project = params.get("project").map(String::as_str);
            let quality = params.get("quality").and_then(|q| q.parse::<u8>().ok()).or(Some(5));
            let note = params.get("note").map(String::as_str);
            match store.stop_session(project, quality, note) {
                Ok(s) => json_ok(&s),
                Err(e) => json_err(400, &e.to_string()),
            }
        }
        _ => json_err(404, "route not found"),
    }
}

fn build_project_zoom(store: &Store, project: &str) -> anyhow::Result<serde_json::Value> {
    let cont = store.project_continue(project)?;
    let status = store.project_status(project)?;
    let stats = store.session_stats(Some(project))?;
    let sessions = store.list_sessions(Some(project), 15)?;
    let hubs = store.experimental_hub_importance(project, 15).unwrap_or_default();
    let facts = store.list_facts(Some(project), false, 30)?;
    Ok(json!({
        "continue": cont,
        "status": status,
        "stats": stats,
        "sessions": sessions,
        "hubs": hubs,
        "facts": facts,
    }))
}

fn build_file_zoom(store: &Store, project: &str, target: &str) -> anyhow::Result<serde_json::Value> {
    let slice = store.slice_object(target, Some(project), None, Some(1), Some(120))?;
    let arch = store.project_archaeology(target, Some(project), 10)?;
    let facts = store
        .list_facts(Some(project), false, 50)?
        .into_iter()
        .filter(|f| f.path.as_deref().is_some_and(|p| p.contains(target) || target.contains(p)))
        .collect::<Vec<_>>();
    Ok(json!({
        "slice": slice,
        "archaeology": arch,
        "facts": facts,
    }))
}

fn json_ok<T: serde::Serialize>(val: &T) -> (u16, &'static str, String) {
    match serde_json::to_string(val) {
        Ok(s) => (200, "application/json", s),
        Err(e) => json_err(500, &e.to_string()),
    }
}

fn json_err(status: u16, msg: &str) -> (u16, &'static str, String) {
    (status, "application/json", json!({ "error": msg }).to_string())
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(percent_decode(k), percent_decode(v));
    }
    map
}

fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
                && let Ok(val) = u8::from_str_radix(hex, 16)
            {
                out.push(val);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn open_url_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}


const HUD_HTML: &str = r####"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>VOS Constellation HUD</title>
<style>
:root{--bg:#0b101b;--panel:#131b2e;--border:#24314f;--fg:#e6edf7;--muted:#8b9bb4;--accent:#38bdf8;--warn:#f59e0b;--danger:#f43f5e;--ok:#34d399}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.45 ui-sans-serif,system-ui,sans-serif}
header{display:flex;justify-content:space-between;align-items:center;padding:12px 20px;border-bottom:1px solid var(--border);background:#0f1626}
.grid{display:grid;grid-template-columns:1.1fr 1.2fr;gap:14px;padding:16px 20px;max-width:1500px;margin:0 auto}
.card{background:var(--panel);border:1px solid var(--border);border-radius:10px;padding:14px}
h2,h3{margin:0 0 10px;font-size:15px}
.nodes{display:grid;grid-template-columns:repeat(auto-fill,minmax(210px,1fr));gap:10px}
.node{padding:10px;border:1px solid var(--border);border-radius:8px;cursor:pointer;background:#0f1626;transition:.15s}
.node:hover,.node.active{border-color:var(--accent);transform:translateY(-1px)}
.pill{display:inline-block;padding:2px 7px;border-radius:999px;font-size:11px;font-weight:600;margin-right:4px}
.Easy{background:rgba(52,211,153,.18);color:var(--ok)}.Medium{background:rgba(245,158,11,.18);color:var(--warn)}.Hard{background:rgba(244,63,94,.2);color:var(--danger)}
.btn{background:var(--accent);color:#04111d;border:0;border-radius:6px;padding:6px 11px;font-weight:600;cursor:pointer;font-size:12px}
.btn.sec{background:#1f2a44;color:var(--fg)}
input{background:#0b101b;border:1px solid var(--border);color:var(--fg);border-radius:6px;padding:6px 9px}
pre{background:#090d16;border:1px solid var(--border);padding:10px;border-radius:6px;overflow:auto;max-height:280px;font-size:12px;white-space:pre-wrap}
.row{display:flex;gap:8px;flex-wrap:wrap;align-items:center}
.list a{display:block;color:var(--accent);text-decoration:none;padding:3px 0;font-family:monospace;font-size:12px;cursor:pointer}
.list a:hover{text-decoration:underline}
.muted{color:var(--muted);font-size:12px}
svg{width:100%;height:190px;background:#090d16;border:1px solid var(--border);border-radius:8px;margin-bottom:10px}
@media(max-width:960px){.grid{grid-template-columns:1fr}}
</style>
</head>
<body>
<header>
  <div><strong>VOS Constellation HUD</strong> <span class="muted" id="meta">loading...</span></div>
  <div class="row">
    <button class="btn sec" onclick="loadPortfolio()">Refresh</button>
  </div>
</header>
<div class="grid">
  <div class="card">
    <h2>Portfolio Constellation</h2>
    <svg id="galaxy" viewBox="0 0 600 190"></svg>
    <div id="nodes" class="nodes"></div>
  </div>
  <div class="card">
    <div class="row" style="justify-content:space-between;margin-bottom:10px">
      <h2 id="projTitle" style="margin:0">Select a project</h2>
      <div class="row">
        <button class="btn" onclick="sessionAct('start')">Start Session</button>
        <button class="btn sec" onclick="sessionAct('stop')">Stop Session</button>
      </div>
    </div>
    <div id="projBody" class="muted">Click any constellation star to zoom into hubs, sessions, facts, and files.</div>
    <h3 style="margin-top:14px">Context Compiler (<code>vos pack</code>)</h3>
    <div class="row" style="margin-bottom:8px">
      <input id="packGoal" placeholder="Goal query..." style="flex:1"/>
      <input id="packBudget" type="number" value="2000" style="width:90px"/>
      <button class="btn" onclick="runPack()">Compile Pack</button>
    </div>
    <pre id="packOut" style="display:none"></pre>
    <h3 style="margin-top:14px">File / Symbol / Archaeology Zoom</h3>
    <pre id="fileOut">Select a hub or resume file above.</pre>
  </div>
</div>
<script>
let curProj = null;
async function api(u, opt){ const r = await fetch(u, opt); return r.json(); }
async function loadPortfolio(){
  const p = await api('/api/portfolio');
  document.getElementById('meta').textContent =
    `${p.projects.length} projects | ${p.total_objects} objects | ${p.total_dirty_files} dirty | ${p.total_stale_facts} stale | cal ${p.calibration.total_scored_sessions}s`;
  const svg = document.getElementById('galaxy');
  const cx = 300, cy = 95, n = Math.max(1, p.projects.length);
  svg.innerHTML = p.projects.map((pr, i) => {
    const a = (i / n) * Math.PI * 2;
    const rad = 52 + (i % 2) * 26;
    const x = cx + Math.cos(a) * rad * 2.2, y = cy + Math.sin(a) * rad;
    const col = pr.dirty_files > 0 ? '#38bdf8' : pr.predicted_difficulty === 'Hard' ? '#f43f5e' : '#34d399';
    const r = Math.min(18, 6 + Math.sqrt(pr.active_objects || 1));
    return `<g style="cursor:pointer" onclick="zoomProj('${pr.project_name}')"><line x1="${cx}" y1="${cy}" x2="${x}" y2="${y}" stroke="#1e293b"/><circle cx="${x}" cy="${y}" r="${r}" fill="${col}" fill-opacity="0.85"/><text x="${x}" y="${y+r+12}" fill="#cbd5e1" font-size="11" text-anchor="middle">${pr.project_name}</text></g>`;
  }).join('');
  document.getElementById('nodes').innerHTML = p.projects.map(pr => `
    <div class="node ${curProj===pr.project_name?'active':''}" onclick="zoomProj('${pr.project_name}')">
      <div><strong>${pr.project_name}</strong> <span class="pill ${pr.predicted_difficulty}">${pr.predicted_difficulty}</span></div>
      <div class="muted">${pr.active_objects} objs | dirty: ${pr.dirty_files} | stale: ${pr.stale_facts_count} | leases: ${pr.active_leases_count}</div>
      <div class="muted">${pr.branch||'-'} · ${(pr.top_areas||[]).join(', ')}</div>
    </div>`).join('');
  if(!curProj && p.projects.length) zoomProj(p.projects[0].project_name);
}
async function zoomProj(name){
  curProj = name;
  document.getElementById('projTitle').textContent = name;
  const d = await api('/api/project?name='+encodeURIComponent(name));
  const hubs = (d.hubs||[]).slice(0,8).map(h=>`<a onclick="zoomFile('${h.path.replace(/'/g,"\\'")}')">${h.path} (deg ${h.incoming_count+h.outgoing_count})</a>`).join('');
  const facts = (d.facts||[]).slice(0,6).map(f=>`<div>• [${f.status}] ${f.fact_text}</div>`).join('');
  document.getElementById('projBody').innerHTML = `
    <div class="muted">Health: ${d.status.health} | Docs: ${d.status.documentation_health} | Sessions: ${d.stats.completed_sessions} | Mean quality: ${(d.stats.avg_recovery_quality||0).toFixed(2)}</div>
    <div style="margin-top:8px"><strong>Top Dependency Hubs</strong><div class="list">${hubs||'<span class="muted">none</span>'}</div></div>
    <div style="margin-top:8px"><strong>Anchored Facts</strong><div class="muted">${facts||'none'}</div></div>`;
}
async function zoomFile(path){
  if(!curProj) return;
  const d = await api(`/api/file?project=${encodeURIComponent(curProj)}&target=${encodeURIComponent(path)}`);
  const syms = (d.slice.symbols||[]).map(s=>`${s.kind} ${s.name} [L${s.start_line}-${s.end_line}]`).join('\n');
  const co = ((d.archaeology&&d.archaeology.co_change_partners)||[]).map(c=>`${c.path} (${c.co_commits}x)`).join(', ');
  document.getElementById('fileOut').textContent =
    `FILE: ${d.slice.path} (${d.slice.total_lines} lines)\nCO-CHANGE: ${co||'none'}\n\nSYMBOLS:\n${syms||'none'}\n\nSLICE:\n${d.slice.content||''}`;
}
async function runPack(){
  if(!curProj) return;
  const g = document.getElementById('packGoal').value || 'architecture overview';
  const b = document.getElementById('packBudget').value || '2000';
  const p = await api(`/api/pack?project=${encodeURIComponent(curProj)}&goal=${encodeURIComponent(g)}&budget=${b}`);
  const el = document.getElementById('packOut');
  el.style.display = 'block';
  el.textContent = p.rendered_markdown || JSON.stringify(p, null, 2);
}
async function sessionAct(kind){
  if(!curProj) return;
  await api(`/api/session/${kind}?project=${encodeURIComponent(curProj)}`, {method:'POST'});
  await zoomProj(curProj); await loadPortfolio();
}
loadPortfolio();
</script>
</body>
</html>"####;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vos_core::ObjectKind;
    use vos_store::UpsertObjectInput;

    #[test]
    fn ui_http_routes_render_html_and_interactive_zoom_json() {
        let tmp = TempDir::new().unwrap();
        let root = vos_core::util::canonicalize_path(tmp.path()).unwrap();
        let file = std::path::Path::new(&root).join("src/app.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "pub fn boot() {}\n").unwrap();

        let mut store = Store::open_in_memory().unwrap();
        let proj = store.create_project("hudproj", Some(std::path::Path::new(&root)), None).unwrap();
        let mut batch = store.begin_index_batch().unwrap();
        batch
            .upsert_object(UpsertObjectInput {
                project_id: Some(proj.id),
                path: file.to_string_lossy().into_owned(),
                kind: ObjectKind::Code,
                title: "app.rs".into(),
                summary: None,
                content_hash: "u1".into(),
                size_bytes: 17,
                modified_at: 100,
                is_noise: false,
                chunks: vec!["pub fn boot() {}".into()],
            })
            .unwrap();
        batch.commit().unwrap();

        let (s0, ct0, html) = dispatch_ui_request(&mut store, "GET", "/");
        assert_eq!(s0, 200);
        assert!(ct0.starts_with("text/html"));
        assert!(html.contains("VOS Constellation HUD"));

        let (s1, _, port_json) = dispatch_ui_request(&mut store, "GET", "/api/portfolio");
        assert_eq!(s1, 200);
        let port: serde_json::Value = serde_json::from_str(&port_json).unwrap();
        assert_eq!(port["projects"][0]["project_name"], "hudproj");

        let (s2, _, proj_json) = dispatch_ui_request(&mut store, "GET", "/api/project?name=hudproj");
        assert_eq!(s2, 200);
        let pj: serde_json::Value = serde_json::from_str(&proj_json).unwrap();
        assert_eq!(pj["status"]["project"]["name"], "hudproj");

        let (s3, _, pack_json) = dispatch_ui_request(&mut store, "GET", "/api/pack?project=hudproj&goal=boot&budget=800");
        assert_eq!(s3, 200);
        let pk: serde_json::Value = serde_json::from_str(&pack_json).unwrap();
        assert!(pk["used_tokens"].as_u64().unwrap() <= 800);

        let (s4, _, _) = dispatch_ui_request(&mut store, "POST", "/api/session/start?project=hudproj&note=hud");
        assert_eq!(s4, 200);
        let (s5, _, _) = dispatch_ui_request(&mut store, "POST", "/api/session/stop?project=hudproj&quality=5");
        assert_eq!(s5, 200);
    }
}
