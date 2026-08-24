> **NOTICE**
>
> `bezel-zed-zlog` is a modified distribution of `zlog` from [zed-industries/zed](https://github.com/zed-industries/zed) — Copyright 2022 - 2025 Zed Industries, Inc., licensed under the Apache License, Version 2.0.
>
> Modified by the bezel project; the modifications and their full source are at [crabtalk/zed](https://github.com/crabtalk/zed), based on upstream `d9ad6a`.

# Zlog

Use the `ZED_LOG` environment variable to control logging output for Zed
applications and libraries. The variable accepts a comma-separated list of
directives that specify logging levels for different modules (crates). The
general format is for instance:

```
ZED_LOG=info,project=debug,agent=off
```

- Levels can be one of: `off`/`none`, `error`, `warn`, `info`, `debug`, or
  `trace`.
- You don't need to specify the global level, default is `trace` in the crate
  and `info` set by `RUST_LOG` in Zed.
