This prompt is the second step of a chain. Its first step changed the spec
at ${SPEC_LOCATION} to meet it, and the spec has been built since. Now change
the code at ${CODE_LOCATION} so it does what the spec now says, without
changing the spec. Follow the spec the prompt references, and the anchors the
spec step names below. Where the spec and the prompt disagree, follow the
spec, and say so in your reply.

The prompt the chain was sent:

${SPEC_PROMPT}

What the spec step said it changed, its final output:

${SPEC_RESULT}
