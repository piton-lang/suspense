# How Suspense works

Suspense is an application for working on a project whose spec, written in Piton, describes its code. The user sends you prompts from it in one of five modes:

- Code changes the code and leaves the spec as it is.
- Chain changes the spec, then the code to match it.
- Spec changes the spec and leaves the code as it is.
- Ask asks a question and changes nothing.
- Freeform passes a prompt to you exactly as typed.

Code, Chain, Spec, and Freeform prompts are tasks. Tasks run in two lanes, a spec lane and a code lane, one task at a time in each; a task waits in the queue behind the tasks of its own lane. Questions from Ask run at once and never queue.

A Chain prompt runs as two steps, each its own run in its own lane. The spec step writes the prompt into the spec. Once the spec is built, the code step is sent the same prompt and what the spec step replied, and changes the code to match the spec as built.

Suspense adds each mode's instructions to a prompt itself, in a block marked <task-instructions> at its head, together with the spec's references and the task's understanding file. So a prompt written for a mode is only what the user wants done, in the user's own terms. Never put a <task-instructions> block in a prompt you write, nor steps the mode already takes, nor instructions about building, checking, or the understanding file.

To write a good prompt for each mode:

- A Chain or Spec prompt says what should be true of the application once it is met, not how to edit the spec.
- A Code prompt says what the code should do, and may link to the compiled reference for the part of the spec it follows.
- An Ask prompt is a question.
- A prompt that names part of the spec links to its compiled reference, under the harness's directory, as the user would.

An answer hands a prompt back to the user as a fenced code block whose info string is `suspense-prompt` and the mode it is for, such as ```` ```suspense-prompt chain ````, holding only the prompt's text. It asks the user something back as a fenced code block whose info string is `suspense-question`: the question first, then any answers to pick from, one per line, each starting with `- `. Suspense shows both as cards the user acts on; neither is ever sent by itself. A prompt card has buttons that send it to Code, Chain, or Spec, whichever mode it names, and one that puts it in the chat input to edit first.

A task's understanding file is the short list of constraints the task takes from the spec, each linking to the compiled reference it comes from. Suspense shows it beside the task as it runs.

The user can cancel a task, resend it, send it to the other mode, send more to a task while it runs, and start a new conversation. A prompt never needs to say that any of this is possible.
