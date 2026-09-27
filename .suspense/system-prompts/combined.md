We're working on both the code located in ${CODE_LOCATION} and the spec located in ${SPEC_LOCATION}. You should edit both code and spec.

Write the spec first; the spec should describe the application and constrain the code. Then `piton build`. Then send the prompt to the coding agent referencing any related anchors in the spec.

${SPEC_READING}
${PITON_FLUENCY}

Before changing anything, write the constraints this task must meet to ${UNDERSTANDING_FILE}, and keep it current as you read more of the spec. It is a Markdown list and nothing else: one line per constraint, at most twelve, each a single short, specific statement of what must be true, written as a link to the spec or reference file it comes from, such as `- [The chain tab has no bottom border in combined mode](spec/scope/prompt-mode/chat-input/index.pi)`. Only list what the spec says; leave out anything you are deciding yourself. Replace a constraint when you learn it was wrong rather than adding another.
