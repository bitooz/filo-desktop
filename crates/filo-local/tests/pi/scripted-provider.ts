// A stand-in model for checks of the real pi (coding_pi.rs's ignored tests): provider `filo-test`, model `script`,
// whose replies follow `FILO_PI_SCRIPT` (JSON), one per call, by how many replies the conversation has: each a list of
// blocks, `{text}` (`{echo: true}` says the last user message back), `{tool, args}` a tool call, and `{wait: ms}`
// first. It needs no login and calls nothing.
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";

export default function (pi) {
  const script = JSON.parse(process.env.FILO_PI_SCRIPT || "[]");
  pi.registerProvider("filo-test", {
    name: "Filo test",
    baseUrl: "http://127.0.0.1:9",
    apiKey: "none",
    api: "filo-test",
    models: [{ id: "script", name: "Script", reasoning: false, input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 100000, maxTokens: 1000 }],
    streamSimple(model, context) {
      const stream = createAssistantMessageEventStream();
      const said = context.messages.filter((message) => message.role === "assistant").length;
      const lastUser = [...context.messages].reverse().find((message) => message.role === "user");
      const userText = typeof lastUser?.content === "string" ? lastUser.content : (lastUser?.content || []).map((part) => part.text || "").join("");
      const blocks = script[Math.min(said, script.length - 1)] || [{ text: "done" }];
      const output = { role: "assistant", content: [], api: model.api, provider: model.provider, model: model.id, usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, stopReason: "stop", timestamp: Date.now() };
      (async () => {
        stream.push({ type: "start", partial: output });
        for (const block of blocks) {
          if (block.wait) await new Promise((resolve) => setTimeout(resolve, block.wait));
          if (block.text !== undefined || block.echo) {
            const text = block.echo ? `heard: ${userText}` : block.text;
            output.content.push({ type: "text", text });
            const index = output.content.length - 1;
            stream.push({ type: "text_start", contentIndex: index, partial: output });
            stream.push({ type: "text_delta", contentIndex: index, delta: text, partial: output });
            stream.push({ type: "text_end", contentIndex: index, content: text, partial: output });
          }
          if (block.tool) {
            const call = { type: "toolCall", id: `call_${said}_${output.content.length}`, name: block.tool, arguments: block.args || {} };
            output.content.push(call);
            const index = output.content.length - 1;
            stream.push({ type: "toolcall_start", contentIndex: index, partial: output });
            stream.push({ type: "toolcall_end", contentIndex: index, toolCall: call, partial: output });
            output.stopReason = "toolUse";
          }
        }
        stream.push({ type: "done", reason: output.stopReason, message: output });
        stream.end();
      })();
      return stream;
    },
  });
}
