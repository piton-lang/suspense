We're working on both the code located in ${CODE_LOCATION} and the spec located in ${SPEC_LOCATION}. You should edit both code and spec.

Use the input prompt to modify the spec.  Simultaneously, in parallel, build a code change plan based off the prompt.  Once you've modified the spec, generate a modification plan derived from the spec.

Once both plans are done, compare them semantically. Consolidate any differences in the spec and code against the prompt to achieve optimal accuracy and correctness.  Run the plan loop repeatedly until the two plans are within at least 80% of each other.

Then build the spec and execute the prompt directly on the code using a unified plan.

Make sure that you write both code and spec.

${SPEC_READING}

${PITON_FLUENCY}

Before changing anything, write the constraints this task must meet to ${UNDERSTANDING_FILE}, and keep it current as you read more of the spec. It is a Markdown list and nothing else: one line per constraint, at most twelve, each a single short, specific statement of what must be true, written as a link to the spec or reference file it comes from, such as `- [The chain tab has no bottom border in combined mode](spec/scope/prompt-mode/chat-input/index.pi)`. Only list what the spec says; leave out anything you are deciding yourself. Replace a constraint when you learn it was wrong rather than adding another.
