# AI Agent

A chat client for LazyOS (and Windows and Linux) that talks to a large
language model on **Fireworks AI**.

* **Key.** Paste your Fireworks API key when the app asks. In this version it
  is kept in memory only and is asked again each time the app starts; it is
  never written to disk.
* **Chat.** Replies stream as they arrive. *Stop* (or Esc) ends one and *Redo*
  asks for the last answer again. Several
  conversations are kept in the app's own folder (`~/.apps/os.lazy.agent`),
  searchable and exportable as Markdown.
* **Privacy.** Everything you type is sent to Fireworks AI, a third party
  outside this computer. The conversations stay on disk, readable by you only.

Diagnostics (reading the system's logs and filing issues) arrive in later
versions. See `docs/agent.md` and `docs/ai-agent-plan.md` in the source tree.
