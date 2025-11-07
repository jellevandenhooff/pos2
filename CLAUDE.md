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

4. **No emojis or unicode characters** - Don't use emojis or decorative unicode characters in code, tests, or logs

5. **Log messages start with lowercase** - Keep log messages lowercase, similar to error messages

## Code Style

1. **Avoid deep indentation** - Extract nested logic into separate functions to keep code readable
   - Good: Extract helpers and use early returns
   - Bad: 5+ levels of nested if/for/match blocks

## Git Commits

1. **Use 1-line commit messages** - Keep commit messages concise and on a single line
2. **No emojis in commit messages** - Don't use emojis or decorative unicode characters in commit messages

## Documentation

1. **Keep READMEs up to date** - When adding new features or changing existing functionality, update relevant README files
2. **Keep READMEs concise** - Avoid excessive detail that will be distracting. Focus on high-level overview and usage
3. **No implementation details in READMEs** - Don't document internal function calls or detailed step-by-step execution flows
4. **Minimal markup in READMEs** - Avoid excessive markdown formatting like bold (**text**). Plain text is preferred
5. **Use sentence case for headers** - Headers should start with a capital letter but not capitalize every word. "Background update loop" not "Background Update Loop"

## Testing

1. **No obvious assert descriptions** - If it's clear what an assert checks, omit the description message
   - Good: `assert!(output.contains("Version: 1.0"));`
   - Bad: `assert!(output.contains("Version: 1.0"), "expected version 1.0 in output");`
   - Only add descriptions when the assertion is complex or the failure reason isn't obvious

