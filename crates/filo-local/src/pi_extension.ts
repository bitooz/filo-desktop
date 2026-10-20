// Filo's extension for pi (coding_pi.rs), loaded with `-e`: what the turn or round was given in `FILO_PI` (JSON).
//
// - `servers`: MCP servers to plug in (a chat's: Filo's and the user's apps), each `{name, command, args}` over stdio.
//   A pi that has MCP of its own takes them with `pi.registerMcpServer`, their tools declared to the model; an older
//   one (0.84 has no MCP) gets them from here: each server is started, asked for its tools, and every tool registered
//   as `mcp__<server>__<tool>`, its calls passed on as `tools/call`.
// - `mode`: how freely it works. Full access lets everything through. Auto lets reading through, and changes to files
//   in its folders (`roots`); a change elsewhere, and in a coding round (`askShell`) a command that isn't plainly a
//   look (`ls`, `git status`…), is asked about first, on Filo's permission card: an `extension_ui_request` select with
//   the options Allow / Allow always / Deny, which Filo answers. Allow always lets the same program (or folder) through
//   for the rest of the run. `lookOnly` takes away writing and editing.
// - Project trust is never asked here: `project_trust` is left undecided, and Filo answers any dialog it doesn't own
//   at once, so a prompt can't hold a turn.

import { spawn } from "node:child_process";
import * as path from "node:path";

const ALLOW = "Allow";
const ALWAYS = "Allow always";
const DENY = "Deny";
const DENIED = "Not allowed. Find another way, or say what you need and why.";

/** Programs whose commands only look, let through in Auto when every part of a command is one. */
const LOOKS = new Set([
  "ls", "pwd", "cat", "head", "tail", "wc", "grep", "rg", "fd", "file", "stat", "which", "echo", "printf", "date",
  "tree", "du", "df", "sort", "uniq", "cut", "diff", "basename", "dirname", "realpath", "env", "true",
]);
const GIT_LOOKS = new Set(["status", "diff", "log", "show", "branch", "rev-parse", "ls-files", "blame", "remote"]);

function config() {
  try {
    return JSON.parse(process.env.FILO_PI || "{}");
  } catch {
    return {};
  }
}

/** Whether a command only looks: no redirection into a file, no substitution, each part a program that only reads. */
export function onlyLooks(command) {
  const text = String(command || "").replace(/\d?>\s*\/dev\/null|2>&1/g, "");
  if (/[>`]|\$\(/.test(text)) return false;
  const parts = text.split(/&&|\|\||[;|\n]/).map((part) => part.trim()).filter(Boolean);
  return parts.length > 0 && parts.every((part) => {
    const words = part.split(/\s+/);
    const program = path.basename(words[0]);
    if (program === "git") return GIT_LOOKS.has(words[1]);
    if (program === "find") return !/\s-(exec|execdir|delete|ok|fprint)/.test(part);
    return LOOKS.has(program);
  });
}

/** The first program a command runs: what Allow always lets through again. */
function programOf(command) {
  const first = String(command || "").trim().split(/\s+/)[0] || "";
  return path.basename(first);
}

function inside(file, roots) {
  return roots.some((root) => file === root || file.startsWith(root.endsWith(path.sep) ? root : root + path.sep));
}

/** A tool name as pi takes it: letters, digits and `_`. */
function toolName(server, tool) {
  return `mcp__${server}__${tool}`.replace(/[^A-Za-z0-9_]/g, "_");
}

/** One MCP server over stdio: JSON-RPC lines both ways. */
function connect(server) {
  const child = spawn(server.command, server.args || [], { stdio: ["pipe", "pipe", "ignore"] });
  const waiting = new Map();
  let next = 1;
  let buffer = "";
  const fail = (error) => {
    for (const { reject } of waiting.values()) reject(error);
    waiting.clear();
  };
  child.on("error", fail);
  child.on("exit", () => fail(new Error(`${server.name} stopped`)));
  child.stdout.on("data", (chunk) => {
    buffer += chunk.toString("utf8");
    let at;
    while ((at = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, at).trim();
      buffer = buffer.slice(at + 1);
      if (!line) continue;
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        continue;
      }
      if (message.id !== undefined && message.method === undefined) {
        const answer = waiting.get(message.id);
        waiting.delete(message.id);
        if (!answer) continue;
        if (message.error) answer.reject(new Error(message.error.message || JSON.stringify(message.error)));
        else answer.resolve(message.result);
      } else if (message.id !== undefined) {
        // A request of the server's (roots, sampling): not offered, so refused.
        write({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "Not supported" } });
      }
    }
  });
  const write = (message) => {
    if (child.stdin.writable) child.stdin.write(JSON.stringify(message) + "\n");
  };
  const request = (method, params) =>
    new Promise((resolve, reject) => {
      const id = next++;
      waiting.set(id, { resolve, reject });
      write({ jsonrpc: "2.0", id, method, params });
    });
  return { child, request, notify: (method, params) => write({ jsonrpc: "2.0", method, params }), id: () => next };
}

/** Plugs `server` in from here (a pi without MCP): its tools, registered once it has listed them. */
async function bridge(pi, server) {
  const peer = connect(server);
  await peer.request("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "filo-pi", version: "1" },
  });
  peer.notify("notifications/initialized", {});
  let tools = [];
  let cursor;
  do {
    const page = await peer.request("tools/list", cursor ? { cursor } : {});
    tools = tools.concat(page.tools || []);
    cursor = page.nextCursor;
  } while (cursor);
  for (const tool of tools) {
    pi.registerTool({
      name: toolName(server.name, tool.name),
      label: tool.title || tool.name,
      description: tool.description || tool.name,
      parameters: tool.inputSchema && tool.inputSchema.type ? tool.inputSchema : { type: "object", properties: {} },
      async execute(_id, params, signal) {
        const call = peer.request("tools/call", { name: tool.name, arguments: params || {} });
        const asked = peer.id() - 1;
        signal?.addEventListener("abort", () => peer.notify("notifications/cancelled", { requestId: asked }));
        const result = await call;
        const content = (result.content || []).flatMap((part) => {
          if (part.type === "text") return [{ type: "text", text: part.text }];
          if (part.type === "image") return [{ type: "image", data: part.data, mimeType: part.mimeType }];
          if (part.type === "resource" && part.resource?.text) return [{ type: "text", text: part.resource.text }];
          return [];
        });
        if (result.isError) throw new Error(content.map((part) => part.text || "").join("\n") || "The tool failed");
        if (!content.length && result.structuredContent) {
          content.push({ type: "text", text: JSON.stringify(result.structuredContent) });
        }
        return { content: content.length ? content : [{ type: "text", text: "(no output)" }], details: {} };
      },
    });
  }
  return peer;
}

export default async function filo(pi) {
  const filo = config();
  const roots = (filo.roots || []).map((root) => path.resolve(root));
  const full = filo.mode === "full_access";
  const always = { programs: new Set(), folders: new Set() };

  const peers = [];
  for (const server of filo.servers || []) {
    if (typeof pi.registerMcpServer === "function") {
      pi.registerMcpServer(server.name, { command: server.command, args: server.args || [], exposure: "direct", timeout: 900 });
      continue;
    }
    try {
      peers.push(await bridge(pi, server));
    } catch (error) {
      // An app that doesn't start leaves the turn without it; Filo's own is checked by the turn's reply.
      console.error(`filo: ${server.name} didn't start: ${error.message}`);
    }
  }
  pi.on("session_shutdown", () => {
    for (const peer of peers) peer.child.kill();
  });

  pi.on("project_trust", () => ({ trusted: "undecided" }));

  const ask = async (ctx, title, detail) => {
    const choice = await ctx.ui.select(detail ? `${title}\n\n${detail}` : title, [ALLOW, ALWAYS, DENY]);
    return choice === ALWAYS ? "always" : choice === ALLOW ? "once" : "no";
  };

  pi.on("tool_call", async (event, ctx) => {
    const input = event.input || {};
    if (event.toolName === "edit" || event.toolName === "write") {
      if (filo.lookOnly && !full) return { block: true, reason: "This work only looks: it changes no files." };
      if (full) return undefined;
      const file = path.resolve(ctx.cwd, String(input.path || input.file_path || ""));
      const folder = path.dirname(file);
      if (inside(file, roots) || [...always.folders].some((done) => inside(file, [done]))) return undefined;
      const answer = await ask(ctx, `Change ${file}`, "It's outside the folder this works in.");
      if (answer === "always") always.folders.add(folder);
      return answer === "no" ? { block: true, reason: DENIED } : undefined;
    }
    if (event.toolName === "bash" && !full && filo.askShell) {
      const command = String(input.command || "");
      if (onlyLooks(command) || always.programs.has(programOf(command))) return undefined;
      const answer = await ask(ctx, `Run \`${command.trim()}\``, input.description);
      if (answer === "always") always.programs.add(programOf(command));
      return answer === "no" ? { block: true, reason: DENIED } : undefined;
    }
    return undefined;
  });
}
