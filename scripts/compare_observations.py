#!/usr/bin/env python3
"""Build an interactive HTML comparison report for two AHand observations.jsonl files."""

from __future__ import annotations

import argparse
import html
import json
from collections import Counter
from pathlib import Path
from typing import Any


def parse_labeled_path(value: str) -> tuple[str, Path]:
    if ":" in value and not value.startswith("/"):
        label, path = value.split(":", 1)
        return label.strip() or path, Path(path).expanduser()
    path = Path(value).expanduser()
    return path.parent.name or path.name, path


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    with path.open("r", encoding="utf-8") as handle:
        for line_no, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError as exc:
                raise SystemExit(f"{path}:{line_no}: invalid JSON: {exc}") from exc
            if isinstance(value, dict):
                records.append(value)
    return records


def nested(record: dict[str, Any], *keys: str) -> Any:
    value: Any = record
    for key in keys:
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    return value


def text_value(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return json.dumps(value, ensure_ascii=False, sort_keys=True)


def usage(record: dict[str, Any]) -> dict[str, int]:
    raw = nested(record, "llmResponse", "usage")
    if not isinstance(raw, dict):
        return {}
    out: dict[str, int] = {}
    for key in ("inputTokens", "outputTokens", "cachedReadTokens", "thoughtTokens", "totalTokens"):
        value = raw.get(key)
        if isinstance(value, bool):
            continue
        if isinstance(value, (int, float)):
            out[key] = int(value)
    return out


def normalize(label: str, path: Path, records: list[dict[str, Any]]) -> dict[str, Any]:
    rows: list[dict[str, Any]] = []
    kind_counts: Counter[str] = Counter()
    channel_counts: Counter[str] = Counter()
    error_counts: Counter[str] = Counter()

    first_ms = None
    for record in records:
        observed = nested(record, "time", "observedAtMs")
        if isinstance(observed, (int, float)):
            first_ms = int(observed)
            break

    cumulative_tools = 0
    cumulative_tool_starts = 0
    cumulative_llm_chars = 0
    cumulative_llm_events = 0
    cumulative_errors = 0
    cumulative_tokens = 0

    for index, record in enumerate(records, 1):
        kind = text_value(record.get("kind") or "unknown")
        kind_counts[kind] += 1
        seq = record.get("seq")
        if not isinstance(seq, (int, float)):
            seq = index

        observed = nested(record, "time", "observedAtMs")
        if isinstance(observed, (int, float)) and first_ms is not None:
            elapsed_ms = int(observed) - first_ms
        else:
            elapsed_ms = index - 1

        llm_text = text_value(nested(record, "llmResponse", "responseText"))
        text_len = len(llm_text)
        if text_len > 0 or kind in {"llm_message", "llm_call_delta"}:
            cumulative_llm_events += 1
            cumulative_llm_chars += text_len

        tool_name = text_value(nested(record, "toolCall", "toolName"))
        tool_kind = text_value(nested(record, "toolCall", "toolKind"))
        tool_status = text_value(nested(record, "toolCall", "status"))
        tool_call_id = text_value(nested(record, "toolCall", "toolCallId"))
        if kind.startswith("tool_call"):
            cumulative_tools += 1
            if kind == "tool_call_start":
                cumulative_tool_starts += 1
        channel = text_value(nested(record, "llmResponse", "channel"))
        if channel:
            channel_counts[channel] += 1

        error_message = text_value(nested(record, "error", "message"))
        error_code = text_value(nested(record, "error", "code"))
        if kind == "error" or error_message:
            cumulative_errors += 1
            error_counts[error_code or error_message[:80] or "error"] += 1

        u = usage(record)
        if u.get("totalTokens"):
            cumulative_tokens = max(cumulative_tokens, u["totalTokens"])
        elif u:
            cumulative_tokens += u.get("inputTokens", 0) + u.get("outputTokens", 0)

        stream = record.get("stream") if isinstance(record.get("stream"), dict) else {}
        result_parser = text_value(nested(record, "runtime", "resultParser"))
        agent_kind = text_value(nested(record, "agent", "agentKind"))

        rows.append(
            {
                "run": label,
                "index": index,
                "seq": int(seq),
                "kind": kind,
                "elapsedMs": elapsed_ms,
                "elapsedSec": round(elapsed_ms / 1000, 3),
                "observedAtMs": observed if isinstance(observed, (int, float)) else None,
                "agentKind": agent_kind,
                "resultParser": result_parser,
                "channel": channel,
                "textLen": text_len,
                "cumulativeLlmChars": cumulative_llm_chars,
                "cumulativeLlmEvents": cumulative_llm_events,
                "toolName": tool_name,
                "toolKind": tool_kind,
                "toolStatus": tool_status,
                "toolCallId": tool_call_id,
                "cumulativeTools": cumulative_tools,
                "cumulativeToolStarts": cumulative_tool_starts,
                "errorCode": error_code,
                "errorMessage": error_message,
                "cumulativeErrors": cumulative_errors,
                "inputTokens": u.get("inputTokens", 0),
                "outputTokens": u.get("outputTokens", 0),
                "cachedReadTokens": u.get("cachedReadTokens", 0),
                "thoughtTokens": u.get("thoughtTokens", 0),
                "totalTokens": u.get("totalTokens", 0),
                "cumulativeTokens": cumulative_tokens,
                "streamChunkCount": stream.get("chunkCount", 0),
                "streamSourceKind": text_value(stream.get("sourceKind")),
                "summary": summarize_record(record),
            }
        )

    backfill_tool_state(rows)
    tool_counts: Counter[str] = Counter(
        row["toolName"] for row in rows if row["kind"].startswith("tool_call") and row["toolName"]
    )

    return {
        "label": label,
        "path": str(path),
        "fileSize": path.stat().st_size,
        "records": len(records),
        "durationSec": rows[-1]["elapsedSec"] if rows else 0,
        "kindCounts": dict(kind_counts),
        "toolCounts": dict(tool_counts.most_common()),
        "channelCounts": dict(channel_counts),
        "errorCounts": dict(error_counts),
        "rows": rows,
        "summary": {
            "agentKind": first_nonempty(row["agentKind"] for row in rows),
            "resultParser": first_nonempty(row["resultParser"] for row in rows),
            "llmEvents": sum(1 for row in rows if row["textLen"] > 0 or row["kind"] in {"llm_message", "llm_call_delta"}),
            "llmChars": rows[-1]["cumulativeLlmChars"] if rows else 0,
            "toolEvents": rows[-1]["cumulativeTools"] if rows else 0,
            "toolStarts": rows[-1]["cumulativeToolStarts"] if rows else 0,
            "errors": rows[-1]["cumulativeErrors"] if rows else 0,
            "maxTotalTokens": max((row["totalTokens"] for row in rows), default=0),
            "maxCumulativeTokens": max((row["cumulativeTokens"] for row in rows), default=0),
        },
    }


def backfill_tool_state(rows: list[dict[str, Any]]) -> None:
    states: dict[str, dict[str, Any]] = {}
    for row in rows:
        tool_call_id = row.get("toolCallId") or ""
        if not tool_call_id:
            continue
        if row.get("kind") == "tool_call_start":
            states[str(tool_call_id)] = {
                "toolName": row.get("toolName") or "",
                "toolKind": row.get("toolKind") or "",
            }
            continue
        state = states.get(str(tool_call_id))
        if not state:
            continue
        if not row.get("toolName") and state.get("toolName"):
            row["toolName"] = state["toolName"]
        if not row.get("toolKind") and state.get("toolKind"):
            row["toolKind"] = state["toolKind"]


def first_nonempty(values: Any) -> str:
    for value in values:
        if value:
            return str(value)
    return ""


def summarize_record(record: dict[str, Any]) -> str:
    kind = text_value(record.get("kind") or "unknown")
    if kind.startswith("tool_call"):
        name = text_value(nested(record, "toolCall", "toolName")) or "(unnamed tool)"
        status = text_value(nested(record, "toolCall", "status"))
        return f"{kind}: {name} {status}".strip()
    if kind in {"llm_message", "llm_call_delta"}:
        channel = text_value(nested(record, "llmResponse", "channel")) or "message"
        text = text_value(nested(record, "llmResponse", "responseText")).replace("\n", " ")
        return f"{kind}:{channel}: {text[:180]}"
    if kind == "llm_call_end":
        return f"{kind}: {json.dumps(usage(record), ensure_ascii=False)}"
    if kind == "error":
        return text_value(nested(record, "error", "message"))[:240]
    if kind == "plan_update":
        entries = nested(record, "plan", "entries")
        if isinstance(entries, list):
            return f"plan_update: {len(entries)} entries"
    return kind


def html_report(payload: dict[str, Any]) -> str:
    data = json.dumps(payload, ensure_ascii=False)
    title = html.escape(payload["title"])
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{
  --bg: #f7f7f4;
  --ink: #1f2328;
  --muted: #667085;
  --line: #d8d8d0;
  --panel: #ffffff;
  --codex: #2563eb;
  --hermes: #c2410c;
  --ok: #16803c;
  --warn: #b45309;
  --err: #b42318;
}}
body {{
  margin: 0;
  background: var(--bg);
  color: var(--ink);
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}}
header {{
  padding: 18px 24px 12px;
  border-bottom: 1px solid var(--line);
  background: #fff;
  position: sticky;
  top: 0;
  z-index: 10;
}}
h1 {{ margin: 0 0 8px; font-size: 20px; }}
.sub {{ color: var(--muted); font-size: 13px; }}
main {{ padding: 18px 24px 40px; }}
.grid {{ display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px; }}
.compare-grid {{ display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px; align-items: start; }}
.cards {{ display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 10px; margin: 14px 0; }}
.card, section {{
  background: var(--panel);
  border: 1px solid var(--line);
  border-radius: 8px;
  padding: 12px;
}}
.card .label {{ color: var(--muted); font-size: 12px; }}
.card .value {{ font-size: 22px; font-weight: 650; margin-top: 3px; }}
section {{ margin-top: 14px; }}
h2 {{ font-size: 15px; margin: 0 0 10px; }}
.controls {{ display: flex; gap: 10px; flex-wrap: wrap; align-items: end; margin-bottom: 10px; }}
label {{ font-size: 12px; color: var(--muted); display: grid; gap: 4px; }}
select, input {{
  border: 1px solid var(--line);
  border-radius: 6px;
  padding: 7px 8px;
  background: #fff;
  color: var(--ink);
}}
svg {{ width: 100%; height: 520px; display: block; background: #fff; border: 1px solid var(--line); border-radius: 8px; }}
.small svg {{ height: 320px; }}
table {{ width: 100%; border-collapse: collapse; font-size: 12px; }}
th, td {{ text-align: left; border-bottom: 1px solid var(--line); padding: 7px 8px; vertical-align: top; }}
th {{ color: var(--muted); font-weight: 600; background: #fafafa; position: sticky; top: 72px; }}
.pill {{ display: inline-block; padding: 2px 6px; border-radius: 999px; background: #eef2ff; font-size: 11px; }}
.tooltip {{
  position: fixed;
  pointer-events: none;
  z-index: 20;
  max-width: 560px;
  background: #111827;
  color: #fff;
  border-radius: 6px;
  padding: 9px 10px;
  font-size: 12px;
  line-height: 1.35;
  box-shadow: 0 10px 24px rgba(0,0,0,.2);
  display: none;
  white-space: pre-wrap;
}}
.legend {{ display: flex; gap: 14px; font-size: 12px; color: var(--muted); margin: 8px 0; flex-wrap: wrap; }}
.dot {{ width: 9px; height: 9px; display: inline-block; border-radius: 50%; margin-right: 5px; }}
.mono {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }}
.run-title {{ display: flex; justify-content: space-between; gap: 8px; align-items: baseline; margin: 0 0 8px; }}
.run-title h3 {{ margin: 0; font-size: 14px; }}
.metric-table th {{ width: 38%; }}
@media (max-width: 900px) {{ .grid, .compare-grid, .cards {{ grid-template-columns: 1fr; }} }}
</style>
</head>
<body>
<header>
  <h1>{title}</h1>
  <div class="sub">Interactive observer comparison. Toggle the X axis to inspect real time, event sequence, cumulative tool calls, cumulative LLM output, or token usage.</div>
</header>
<main>
  <div id="summary"></div>

  <section>
    <h2>Event Timeline</h2>
    <div class="controls">
      <label>X axis
        <select id="xAxis">
          <option value="elapsedSec">elapsed seconds</option>
          <option value="seq">record seq</option>
          <option value="index">record index</option>
          <option value="cumulativeTools">cumulative tool events</option>
          <option value="cumulativeToolStarts">cumulative tool starts</option>
          <option value="cumulativeLlmChars">cumulative LLM chars</option>
          <option value="cumulativeLlmEvents">cumulative LLM events</option>
          <option value="cumulativeTokens">cumulative tokens</option>
        </select>
      </label>
      <label>Y mode
        <select id="yMode">
          <option value="lanes">event lanes</option>
          <option value="cumulative">cumulative counts</option>
          <option value="tokens">token usage + tool spans</option>
        </select>
      </label>
      <label>Kind filter
        <select id="kindFilter"><option value="">all kinds</option></select>
      </label>
      <label>Tool filter
        <input id="toolFilter" placeholder="substring, e.g. youtube">
      </label>
    </div>
    <div class="legend" id="legend"></div>
    <svg id="timeline"></svg>
  </section>

  <section>
    <h2>Observation Kinds</h2>
    <div class="compare-grid">
      <section class="small">
        <div class="run-title"><h3 id="kindTitle0"></h3><span class="sub">left</span></div>
        <svg id="kindChart0"></svg>
      </section>
      <section class="small">
        <div class="run-title"><h3 id="kindTitle1"></h3><span class="sub">right</span></div>
        <svg id="kindChart1"></svg>
      </section>
    </div>
  </section>

  <section>
    <h2>Top Tools</h2>
    <div class="compare-grid">
      <section class="small">
        <div class="run-title"><h3 id="toolTitle0"></h3><span class="sub">left</span></div>
        <svg id="toolChart0"></svg>
      </section>
      <section class="small">
        <div class="run-title"><h3 id="toolTitle1"></h3><span class="sub">right</span></div>
        <svg id="toolChart1"></svg>
      </section>
    </div>
  </section>

  <section>
    <h2>Selected Events</h2>
    <div class="compare-grid">
      <section>
        <div class="run-title"><h3 id="eventsTitle0"></h3><span class="sub">left</span></div>
        <table id="eventsTable0"></table>
      </section>
      <section>
        <div class="run-title"><h3 id="eventsTitle1"></h3><span class="sub">right</span></div>
        <table id="eventsTable1"></table>
      </section>
    </div>
  </section>
</main>
<div class="tooltip" id="tooltip"></div>
<script>
const DATA = {data};
const COLORS = ["#2563eb", "#c2410c", "#16803c", "#7c3aed"];
const KIND_LANES = {{
  "agent_session": 1, "status": 1, "raw": 1,
  "llm_call_start": 2, "llm_message": 2, "llm_call_delta": 2, "llm_call_end": 2,
  "tool_call_start": 3, "tool_call_output": 3.5, "tool_call_end": 4,
  "plan_update": 4.5, "error": 5, "parse_error": 5
}};
const RUN_COLOR = Object.fromEntries(DATA.runs.map((r, i) => [r.label, COLORS[i % COLORS.length]]));
const rows = DATA.runs.flatMap(r => r.rows);
const tooltip = document.getElementById("tooltip");

function fmt(n) {{
  if (n === null || n === undefined || Number.isNaN(n)) return "";
  return Number(n).toLocaleString();
}}
function esc(s) {{
  return String(s ?? "").replace(/[&<>"']/g, c => ({{"&":"&amp;","<":"&lt;",">":"&gt;","\\"":"&quot;","'":"&#39;"}}[c]));
}}
function extent(values) {{
  const nums = values.filter(v => typeof v === "number" && Number.isFinite(v));
  if (!nums.length) return [0, 1];
  let min = Math.min(...nums), max = Math.max(...nums);
  if (min === max) {{ min -= 1; max += 1; }}
  return [min, max];
}}
function scale(value, domain, range) {{
  return range[0] + (value - domain[0]) * (range[1] - range[0]) / (domain[1] - domain[0]);
}}
function kindLane(kind) {{ return KIND_LANES[kind] || 0.5; }}
function eventRadius(row) {{
  if (row.kind === "error") return 7;
  if (row.kind.startsWith("tool_call")) return 5;
  if (row.textLen > 0) return Math.max(4, Math.min(11, Math.sqrt(row.textLen) / 2.1));
  return 4;
}}
function eventShape(row) {{
  if (row.kind === "error") return "triangle";
  if (row.kind.startsWith("tool_call")) return "square";
  if (row.kind.startsWith("llm")) return "circle";
  return "diamond";
}}
function tooltipText(row) {{
  return [
    `${{row.run}} #${{row.seq}} ${{row.kind}}`,
    `x elapsed=${{row.elapsedSec}}s index=${{row.index}}`,
    row.toolName ? `tool=${{row.toolName}} (${{row.toolStatus || ""}})` : "",
    row.channel ? `channel=${{row.channel}} textLen=${{row.textLen}} chunks=${{row.streamChunkCount || 0}}` : row.textLen ? `textLen=${{row.textLen}}` : "",
    row.totalTokens ? `tokens total=${{row.totalTokens}} in=${{row.inputTokens}} out=${{row.outputTokens}} cached=${{row.cachedReadTokens}}` : "",
    row.errorMessage ? `error=${{row.errorCode || ""}} ${{row.errorMessage}}` : "",
    row.summary || ""
  ].filter(Boolean).join("\\n");
}}
function showTip(evt, text) {{
  tooltip.textContent = text;
  tooltip.style.left = Math.min(evt.clientX + 14, window.innerWidth - 590) + "px";
  tooltip.style.top = (evt.clientY + 14) + "px";
  tooltip.style.display = "block";
}}
function hideTip() {{ tooltip.style.display = "none"; }}
function clear(svg) {{ while (svg.firstChild) svg.removeChild(svg.firstChild); }}
function el(name, attrs = {{}}, text = "") {{
  const node = document.createElementNS("http://www.w3.org/2000/svg", name);
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
  if (text) node.textContent = text;
  return node;
}}
function drawMarker(svg, row, x, y) {{
  const color = row.kind === "error" ? "#b42318" : RUN_COLOR[row.run];
  const r = eventRadius(row);
  let node;
  const shape = eventShape(row);
  if (shape === "square") {{
    node = el("rect", {{x: x - r, y: y - r, width: r * 2, height: r * 2, fill: color, opacity: .78, rx: 2}});
  }} else if (shape === "triangle") {{
    node = el("path", {{d: `M ${{x}} ${{y-r-1}} L ${{x-r-1}} ${{y+r}} L ${{x+r+1}} ${{y+r}} Z`, fill: color, opacity: .9}});
  }} else if (shape === "diamond") {{
    node = el("path", {{d: `M ${{x}} ${{y-r}} L ${{x+r}} ${{y}} L ${{x}} ${{y+r}} L ${{x-r}} ${{y}} Z`, fill: color, opacity: .72}});
  }} else {{
    node = el("circle", {{cx: x, cy: y, r, fill: color, opacity: .78}});
  }}
  node.addEventListener("mousemove", evt => showTip(evt, tooltipText(row)));
  node.addEventListener("mouseleave", hideTip);
  svg.appendChild(node);
}}
function toolSpans(data, xKey) {{
  const starts = new Map();
  const spans = [];
  for (const row of data) {{
    const id = row.toolCallId || "";
    if (!id) continue;
    if (row.kind === "tool_call_start") {{
      starts.set(id, row);
    }} else if ((row.kind === "tool_call_end" || row.kind === "tool_call_output") && starts.has(id)) {{
      const start = starts.get(id);
      spans.push({{
        id,
        run: row.run,
        toolName: row.toolName || start.toolName || "(unnamed tool)",
        toolKind: row.toolKind || start.toolKind || "",
        status: row.toolStatus || "",
        start,
        end: row,
        x0: Number(start[xKey] || 0),
        x1: Number(row[xKey] || 0),
        y: Math.max(start.cumulativeTokens || 0, row.cumulativeTokens || 0),
      }});
      if (row.kind === "tool_call_end") starts.delete(id);
    }}
  }}
  return spans;
}}
function drawToolSpan(svg, span, xScale, yScale) {{
  const x0 = xScale(span.x0);
  const x1 = xScale(span.x1);
  const y = yScale(span.y);
  const color = RUN_COLOR[span.run] || "#2563eb";
  const line = el("line", {{
    x1: Math.min(x0, x1),
    x2: Math.max(x0, x1),
    y1: y,
    y2: y,
    stroke: color,
    "stroke-width": 5,
    "stroke-linecap": "round",
    opacity: .48,
  }});
  line.addEventListener("mousemove", evt => showTip(evt, [
    `${{span.run}} tool span`,
    `tool=${{span.toolName}}`,
    `id=${{span.id}} status=${{span.status}}`,
    `x=${{span.x0}} -> ${{span.x1}}`,
    `token y=${{span.y}}`,
  ].join("\\n")));
  line.addEventListener("mouseleave", hideTip);
  svg.appendChild(line);
}}
function filteredRows() {{
  const kind = document.getElementById("kindFilter").value;
  const tool = document.getElementById("toolFilter").value.toLowerCase().trim();
  return rows.filter(r => (!kind || r.kind === kind) && (!tool || (r.toolName || "").toLowerCase().includes(tool)));
}}
function runRows(run) {{
  const kind = document.getElementById("kindFilter").value;
  const tool = document.getElementById("toolFilter").value.toLowerCase().trim();
  return run.rows.filter(r => (!kind || r.kind === kind) && (!tool || (r.toolName || "").toLowerCase().includes(tool)));
}}
function drawTimeline() {{
  const svg = document.getElementById("timeline");
  clear(svg);
  const width = svg.clientWidth || 1100, height = svg.clientHeight || 520;
  svg.setAttribute("viewBox", `0 0 ${{width}} ${{height}}`);
  const margin = {{left: 64, right: 28, top: 22, bottom: 44}};
  const xKey = document.getElementById("xAxis").value;
  const yMode = document.getElementById("yMode").value;
  const data = filteredRows();
  const spans = yMode === "tokens" ? toolSpans(data, xKey) : [];
  const xDomain = extent(data.map(r => Number(r[xKey] || 0)).concat(spans.flatMap(s => [s.x0, s.x1])));
  const yValues = yMode === "lanes"
    ? data.map(r => kindLane(r.kind))
    : yMode === "tokens"
      ? data.map(r => r.cumulativeTokens || r.totalTokens || 0).concat(spans.map(s => s.y))
      : data.map(r => r.cumulativeTools + r.cumulativeLlmEvents + r.cumulativeErrors);
  const yDomain = extent(yValues);

  for (let i = 0; i <= 6; i++) {{
    const x = margin.left + i * (width - margin.left - margin.right) / 6;
    svg.appendChild(el("line", {{x1:x, x2:x, y1:margin.top, y2:height-margin.bottom, stroke:"#ecece6"}}));
    const val = xDomain[0] + i * (xDomain[1] - xDomain[0]) / 6;
    svg.appendChild(el("text", {{x, y:height-18, "text-anchor":"middle", fill:"#667085", "font-size":11}}, fmt(Math.round(val))));
  }}
  if (yMode === "lanes") {{
    const labels = [["session",1],["llm",2],["tool start/out",3],["tool end",4],["plan",4.5],["error",5]];
    for (const [label, lane] of labels) {{
      const y = scale(lane, [0.5,5.2], [height-margin.bottom, margin.top]);
      svg.appendChild(el("line", {{x1:margin.left, x2:width-margin.right, y1:y, y2:y, stroke:"#eeeeea"}}));
      svg.appendChild(el("text", {{x:8, y:y+4, fill:"#667085", "font-size":11}}, label));
    }}
  }} else {{
    for (let i = 0; i <= 5; i++) {{
      const y = margin.top + i * (height - margin.top - margin.bottom) / 5;
      svg.appendChild(el("line", {{x1:margin.left, x2:width-margin.right, y1:y, y2:y, stroke:"#eeeeea"}}));
      const val = yDomain[1] - i * (yDomain[1] - yDomain[0]) / 5;
      svg.appendChild(el("text", {{x:8, y:y+4, fill:"#667085", "font-size":11}}, fmt(Math.round(val))));
    }}
  }}
  svg.appendChild(el("line", {{x1:margin.left, x2:width-margin.right, y1:height-margin.bottom, y2:height-margin.bottom, stroke:"#98a2b3"}}));
  svg.appendChild(el("line", {{x1:margin.left, x2:margin.left, y1:margin.top, y2:height-margin.bottom, stroke:"#98a2b3"}}));

  if (yMode === "tokens") {{
    const xScale = value => scale(value, xDomain, [margin.left, width-margin.right]);
    const yScale = value => scale(value, yDomain, [height-margin.bottom, margin.top]);
    for (const span of spans) drawToolSpan(svg, span, xScale, yScale);
  }}

  for (const row of data) {{
    const x = scale(Number(row[xKey] || 0), xDomain, [margin.left, width-margin.right]);
    const yValue = yMode === "lanes"
      ? kindLane(row.kind)
      : yMode === "tokens"
        ? (row.cumulativeTokens || row.totalTokens || 0)
        : row.cumulativeTools + row.cumulativeLlmEvents + row.cumulativeErrors;
    const y = scale(yValue, yMode === "lanes" ? [0.5,5.2] : yDomain, [height-margin.bottom, margin.top]);
    drawMarker(svg, row, x, y);
  }}
}}
function drawTimelines() {{
  drawTimeline();
  drawTables();
}}
function drawBarChart(svgId, items, color, valueLabel="count") {{
  const svg = document.getElementById(svgId);
  clear(svg);
  const width = svg.clientWidth || 520, height = svg.clientHeight || 300;
  svg.setAttribute("viewBox", `0 0 ${{width}} ${{height}}`);
  const margin = {{left: 150, right: 24, top: 16, bottom: 20}};
  const max = Math.max(1, ...items.map(x => x.value));
  const rowH = Math.max(18, (height - margin.top - margin.bottom) / Math.max(1, items.length));
  items.forEach((item, i) => {{
    const y = margin.top + i * rowH;
    const w = (width - margin.left - margin.right) * item.value / max;
    svg.appendChild(el("text", {{x:8, y:y+rowH*.65, fill:"#344054", "font-size":11}}, item.label.slice(0, 24)));
    svg.appendChild(el("rect", {{x:margin.left, y:y+3, width:w, height:Math.max(8,rowH-7), fill:color || item.color || "#2563eb", opacity:.78, rx:3}}));
    svg.appendChild(el("text", {{x:margin.left+w+5, y:y+rowH*.65, fill:"#667085", "font-size":11}}, `${{fmt(item.value)}} ${{valueLabel}}`));
  }});
}}
function drawSmallCharts() {{
  DATA.runs.forEach((run, index) => {{
    const kindItems = Object.entries(run.kindCounts)
      .map(([label, value]) => ({{label, value}}))
      .sort((a,b)=>b.value-a.value)
      .slice(0,18);
    drawBarChart(`kindChart${{index}}`, kindItems, RUN_COLOR[run.label]);
    const toolItems = Object.entries(run.toolCounts)
      .map(([label, value]) => ({{label, value}}))
      .sort((a,b)=>b.value-a.value)
      .slice(0,18);
    drawBarChart(`toolChart${{index}}`, toolItems, RUN_COLOR[run.label]);
  }});
}}
function drawSummary() {{
  const root = document.getElementById("summary");
  root.innerHTML = `<section><h2>Comparison Summary</h2>${{comparisonTable()}}</section><div class="compare-grid">${{DATA.runs.map((run, i) => `
    <section>
      <h2>${{esc(run.label)}}</h2>
      <div class="sub mono">${{esc(run.path)}}</div>
      <div class="cards">
        <div class="card"><div class="label">records</div><div class="value">${{fmt(run.records)}}</div></div>
        <div class="card"><div class="label">duration sec</div><div class="value">${{fmt(run.durationSec)}}</div></div>
        <div class="card"><div class="label">tool starts</div><div class="value">${{fmt(run.summary.toolStarts)}}</div></div>
        <div class="card"><div class="label">errors</div><div class="value">${{fmt(run.summary.errors)}}</div></div>
      </div>
      <table>
        <tr><th>agent</th><td>${{esc(run.summary.agentKind || "")}}</td></tr>
        <tr><th>parser</th><td>${{esc(run.summary.resultParser || "")}}</td></tr>
        <tr><th>LLM events/chars</th><td>${{fmt(run.summary.llmEvents)}} / ${{fmt(run.summary.llmChars)}}</td></tr>
        <tr><th>tokens</th><td>max total=${{fmt(run.summary.maxTotalTokens)}} cumulative=${{fmt(run.summary.maxCumulativeTokens)}}</td></tr>
      </table>
    </section>`).join("")}}</div>`;
  document.getElementById("legend").innerHTML = DATA.runs.map(r => `<span><span class="dot" style="background:${{RUN_COLOR[r.label]}}"></span>${{esc(r.label)}}</span>`).join("") +
    `<span><span class="dot" style="background:#b42318"></span>error</span>`;
  DATA.runs.forEach((run, index) => {{
    for (const prefix of ["kindTitle", "toolTitle", "eventsTitle"]) {{
      const node = document.getElementById(`${{prefix}}${{index}}`);
      if (node) node.textContent = run.label;
    }}
  }});
}}
function comparisonTable() {{
  const [left, right] = DATA.runs;
  const metrics = [
    ["agent", left.summary.agentKind || "", right.summary.agentKind || ""],
    ["parser", left.summary.resultParser || "", right.summary.resultParser || ""],
    ["records", left.records, right.records],
    ["duration sec", left.durationSec, right.durationSec],
    ["LLM events", left.summary.llmEvents, right.summary.llmEvents],
    ["LLM chars", left.summary.llmChars, right.summary.llmChars],
    ["tool starts", left.summary.toolStarts, right.summary.toolStarts],
    ["tool events", left.summary.toolEvents, right.summary.toolEvents],
    ["errors", left.summary.errors, right.summary.errors],
    ["max total tokens", left.summary.maxTotalTokens, right.summary.maxTotalTokens],
    ["cumulative tokens", left.summary.maxCumulativeTokens, right.summary.maxCumulativeTokens],
  ];
  return `<table class="metric-table"><thead><tr><th>metric</th><th>${{esc(left.label)}}</th><th>${{esc(right.label)}}</th><th>delta right-left</th></tr></thead><tbody>` +
    metrics.map(([name, l, r]) => {{
      const delta = (typeof l === "number" && typeof r === "number") ? fmt(r-l) : "";
      return `<tr><th>${{esc(name)}}</th><td>${{esc(typeof l === "number" ? fmt(l) : l)}}</td><td>${{esc(typeof r === "number" ? fmt(r) : r)}}</td><td>${{esc(delta)}}</td></tr>`;
    }}).join("") + `</tbody></table>`;
}}
function initFilters() {{
  const kinds = [...new Set(rows.map(r => r.kind))].sort();
  const select = document.getElementById("kindFilter");
  for (const kind of kinds) {{
    const opt = document.createElement("option");
    opt.value = kind; opt.textContent = kind;
    select.appendChild(opt);
  }}
}}
function drawTableForRun(run, index) {{
  const data = runRows(run).slice(-80).reverse();
  const table = document.getElementById(`eventsTable${{index}}`);
  const cols = ["run","seq","elapsedSec","kind","toolName","toolStatus","channel","textLen","totalTokens","summary"];
  table.innerHTML = `<thead><tr>${{cols.map(c=>`<th>${{c}}</th>`).join("")}}</tr></thead><tbody>` +
    data.map(row => `<tr>${{cols.map(c=>`<td>${{esc(row[c] ?? "")}}</td>`).join("")}}</tr>`).join("") +
    `</tbody>`;
}}
function drawTables() {{ DATA.runs.forEach((run, index) => drawTableForRun(run, index)); }}
function redraw() {{ drawTimelines(); drawSmallCharts(); }}
drawSummary();
initFilters();
for (const id of ["xAxis","yMode","kindFilter","toolFilter"]) document.getElementById(id).addEventListener("input", redraw);
window.addEventListener("resize", redraw);
redraw();
</script>
</body>
</html>"""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--left", required=True, help="label:/path/to/observations.jsonl")
    parser.add_argument("--right", required=True, help="label:/path/to/observations.jsonl")
    parser.add_argument("--out", required=True, help="output HTML path")
    parser.add_argument("--title", default="AHand Observation Comparison")
    parser.add_argument("--summary-json", help="optional machine-readable summary output")
    args = parser.parse_args()

    left_label, left_path = parse_labeled_path(args.left)
    right_label, right_path = parse_labeled_path(args.right)
    for path in (left_path, right_path):
        if not path.is_file():
            raise SystemExit(f"not found: {path}")

    payload = {
        "title": args.title,
        "runs": [
            normalize(left_label, left_path, read_jsonl(left_path)),
            normalize(right_label, right_path, read_jsonl(right_path)),
        ],
    }

    out = Path(args.out).expanduser()
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(html_report(payload), encoding="utf-8")

    if args.summary_json:
        summary = Path(args.summary_json).expanduser()
        summary.parent.mkdir(parents=True, exist_ok=True)
        compact = {
            "title": payload["title"],
            "runs": [
                {key: run[key] for key in ("label", "path", "records", "durationSec", "kindCounts", "toolCounts", "channelCounts", "errorCounts", "summary")}
                for run in payload["runs"]
            ],
        }
        summary.write_text(json.dumps(compact, ensure_ascii=False, indent=2), encoding="utf-8")

    print(f"wrote {out}")


if __name__ == "__main__":
    main()
