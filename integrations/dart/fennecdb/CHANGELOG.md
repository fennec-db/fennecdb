# Changelog

## Unreleased

Changed before 1.0, which freezes the API:

- `tokenProvider` is handed a `TokenRefusal` -- `errorCode`, `expired`,
  `hadToken`, `attempt` -- and a function of no argument is no longer taken:
  write `(_) => refresh()` where the provider reads none of it.
- A 401 never asks the app to sign out: the provider asks its issuer for a
  fresh token at every one, and signs out where the issuer refuses. Where
  the token handed over is refused too (`attempt` past 1), the sync asks
  again after 1 s doubling to 60 s; an expiry of the token the server took
  last, or of the first handed over since, starts the count at 1 again, and
  only a 2xx to the token held, or a stream that opens, resets it.
- `resume()` and `setOnline(true)` ask again for a token a provider that
  failed did not hand over -- never while it is still at work: a provider
  is called once at a time. A provider that fails, or hands over an empty
  token, is told in `replica.status` (`token` `TokenStage.failed`, the
  error "the token provider failed: …"), where it was swallowed; an empty
  token is no longer handed to the server as none.
- `SyncStatus.token`, a `TokenWait` -- its `TokenStage` and the
  `TokenRefusal` -- says what the sync waits on a token for while it does,
  `SyncFailure.errorCode` the code a 401 named, and
  `FennecException.errorCode` is a refusal's code.
- A replica is one account's: its file's path names the account, and it is
  never opened for another.

## 0.2.0

The release notes: https://github.com/fennec-db/fennecdb/releases/tag/v0.2.0

## 0.1.12

The release notes: https://github.com/fenecdb/fenecdb/releases/tag/v0.1.12

## 0.1.11

The release notes: https://github.com/fenecdb/fenecdb/releases/tag/v0.1.11

## 0.1.10

The release notes: https://github.com/fenecdb/fenecdb/releases/tag/v0.1.10

## 0.1.9

The release notes: https://github.com/fenecdb/fenecdb/releases/tag/v0.1.9

## 0.1.8

The release notes: https://github.com/fenecdb/fenecdb/releases/tag/v0.1.8

## 0.1.7

The first release: a database in a file through the native library, FenecQL
and the query builder, live queries as Streams.
