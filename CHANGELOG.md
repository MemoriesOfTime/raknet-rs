# Changelog

## Unreleased

- Split `IO` into `Stream` and `Sink`
- Fix some fragmentation bugs
- Add `FlushStrategy`

---
## 0.1.3

- Preliminary implementing RFC6298 RttEstimator
- Zero copy packets decoding
- Support offline packets lifetime span tracing
- Add experimental `Ping` for clients
- Reduced the overhead when calling `poll_close`
  - By introducing a simple time reactor
- Add examples for E2E tracing
- Fixed some API call issues

---
## 0.1.2

- Fix fragmented packets boundary
- Refactor lots of codes
- Ensure all packets are delivered after `poll_close`
- Add packet tracing powered by `minitrace`

---
## 0.1.1

- Add `tokio-udp` feature
