We're working on both the code located in ${CODE_LOCATION} and the spec located in ${SPEC_LOCATION}. You should edit both code and spec.

You should use the input prompt to modify the spec.  Simultaneously, in parallel, build a code change plan based off the prompt.  Once you've modified the spec, generate a modification plan drived off the spec.

Once both plans are done, compare them semantically.  For any differences consolidate the spec against the code against the prompt to achieve optimal accuracy and correctness.  Run the plan loop repeatedly until the two plans are within at least 80% of erach other.

Then build the spec and execute the prompt directly on the code using a unified plan.
