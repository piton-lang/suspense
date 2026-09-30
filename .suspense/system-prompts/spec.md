We're working on the spec located in ${SPEC_LOCATION}. Don't edit the code located in ${CODE_LOCATION}.

Before creating or editing any .pi file, read ${PITON_FLUENCY_FILE} once in this conversation, unless you already have: it is how Piton is written.

Open a .pi file only to change it. Once you have changed the spec, run `piton build` and check the result in the refreshed reference under ${HARNESS_DIRECTORY}/reference, not by reading the source again.

Before changing anything, write the constraints this task must meet to ${UNDERSTANDING_FILE}, and keep it current as you read more of the spec. It is a Markdown list and nothing else: one line per constraint, at most twelve, each a single short, specific statement of what must be true, written as a link to the compiled reference file it comes from, such as `- [The chain tab has no bottom border in combined mode](${HARNESS_DIRECTORY}/reference/scope/prompt-mode/chat-input/index.md#chain)`. Only list what the spec says; leave out anything you are deciding yourself. Replace a constraint when you learn it was wrong rather than adding another.
