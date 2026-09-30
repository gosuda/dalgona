# Initializer and sprints

## When to use this

Use this recipe when the task outlives one context window.

## Initializer pass

Set up once:

- install the required tools and dependencies;
- expand the specification into a feature-list file;
- create a progress file;
- make the initial commit.

Do not repeat the initializer pass in a sprint.

## Sprint loop

Work on one feature per session:

1. Read the progress file before planning.
2. Pick the next unfinished feature from the feature-list file.
3. Implement that feature and keep the change inside its contract.
4. Run the end-to-end check for the changed path.
5. Update the progress file with the result and the next feature.
6. Commit the completed slice with a message that names the change.

## The victory rule

Never declare the build done without running the dev entry point and exercising the changed path.

## Files

Keep the feature-list file and the progress file in the workspace root. Keep both files as plain Markdown. The feature-list file records the complete work expanded from the specification. The progress file records what the last session completed, what failed, and what the next session must do.
