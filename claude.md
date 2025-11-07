# Coding Rules

## Error Handling

1. **Error messages start with lowercase** - All error messages should begin with a lowercase letter
   - Good: `anyhow::bail!("could not determine current image SHA")`
   - Bad: `anyhow::bail!("Could not determine current image SHA")`

2. **Preserve original errors when wrapping** - When adding context to errors, use `.context()` or `.with_context()` to keep the original error
   - Good: `do_something().context("failed to do something")?`
   - Bad: Creating a new error without the original cause

## Code Comments and Logging

1. **No redundant comments** - Avoid comments that duplicate what the code or log lines already say
   - Good: `println!("Building version 1");`
   - Bad: `// Build version 1` followed by `println!("Building version 1");`

2. **Logging is okay for runtime visibility** - It's fine to log what code does at runtime since we can't see the execution

3. **Avoid fairly obvious comments** - Don't add comments that don't add value. The code is complex but readers can figure things out

## Git Commits

1. **Use 1-line commit messages** - Keep commit messages concise and on a single line

