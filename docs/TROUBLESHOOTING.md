# When something does not work

Aldo's failure modes are unusually easy to confuse, because most of them look
like the same thing: a search that returns nothing. Four different problems
produce that one symptom, and telling them apart is most of the work.

## Nothing came back from the server

The line begins with `Server:` and ends with a count of folders and files. Read
the count and the sentence after it together.

**`Server: not configured`** — the username or password is missing. Both are
required, and half a credential is reported as nothing rather than as a login
failure. Sign in with:

```sh
aldo login
```

`aldo tui` normally asks on first run; this is the case where the terminal had
no way to prompt. To set them by hand instead:

```sh
export ALDO_SLSK_USERNAME="your-name"
export ALDO_SLSK_PASSWORD="your-password"
```

**`Server: rate limited, retry in …`** — the request was never sent. SoulSeek
allows 34 searches per 220 seconds and bans for 30 minutes if you exceed it, so
Aldo refuses instead of retrying. `aldo budgets` shows the remaining allowance.

**`Server: sign-in rejected`** — the server refused the login. The username or
password is wrong, or the name is already registered and needs *its* exact
password. SoulSeek creates a name only when it is free, and gives the same
answer for "taken" and "wrong password". Check the account, then sign in again:

```sh
aldo whoami     # which username is effective, and whether it comes from the environment or the file
aldo login      # change it
```

**`Server: <error>`** — the client could not connect. This is a real fault, not
an empty result, and it is reported as one.

**`Server: connected; 0 folders, 0 files`** — the login worked and nobody
answered. This is the common case and it is not a bug:

- Results normally come only from peers online during the search. Retry later.
- Give peers longer to reply. The default window is eight seconds, which is thin
  for a rare album: `--slsk-timeout 20`.
- Check what is being shared. Sharing nothing gets this client queued last, and
  the hint says so when it applies. `--slsk-share <dir>` or
  `ALDO_SLSK_SHARED=/home/you/Music`.
- Forward the listen port (2234 by default). Peers reach a client they can
  connect to, and availability improves noticeably.

**`all were rejected by Aldo's lossless-file filters`** — the network did answer.
Peers sent files and every one was dropped, usually because it was not FLAC or
was below the minimum bit depth. The hint names how many. A broader query, or the
same query later, is the fix; this is *not* the same as nobody answering.

## Nothing came back from Discogs

**`Discogs: not configured: set ALDO_DISCOGS_TOKEN`** — there is no token, so the
request was never made. Create one at
<https://www.discogs.com/settings/developers> and save it:

```sh
aldo login --token YOUR-TOKEN
```

That updates only the token, so the SoulSeek account is left as it is.

**`Discogs: rate limited, retry in …`** — 60 requests a minute with a token, 25
without. Aldo fetches a release body for each hit, but caches every body
permanently, so a repeated search costs one request.

**`Discogs: N fetched, M cached, K reported` with no matches listed** — Discogs
answered and had nothing. Check the query: `Surf's Up The Beach Boys` is one
phrase because it has no ` - ` separator, and a phrase is a much weaker match
than `The Beach Boys - Surf's Up`.

## The download refused to start

**`result index #N is unavailable`** — daily indexes reset at UTC midnight, and a
new search replaces them. Run `aldo search` again and use the number it prints.

**`the selected server folder has N files, but the matched Discogs release has M
tracks`** — Aldo will not tag a folder whose file count disagrees with the
release, because that is how a partial album acquires a full album's metadata.
Either pick the right pressing (`--discogs-release`, or `t` in the interface) or
accept it untagged:

```sh
aldo download 3 --force
```

`--force` is the right answer for extended editions, incomplete folders, and
anything Discogs has never heard of.

**`failed to run metaflac`** — the FLAC tools are not installed. `sudo pacman -S
flac`, or the equivalent for `metaflac` on your distribution.

## Reading the two panes

In the interface, `Tab` switches between the server folders and the Discogs
matches, and each pane answers a different question. A folder's `quality` column
repeats what the peer claimed: `quality unknown` means the peer reported no
attributes, not that the file is a low-quality rip. A `.flac` from a client that
reports nothing is still a FLAC.

The two numbers that get confused are the **daily index** (`#3`) and the
**Discogs release id** (`579409`). Only the first is what `aldo download` takes.

## Proxy

A SOCKS5 or HTTP proxy covers the metadata sources — Discogs, MusicBrainz, the
trackers — and nothing else:

```sh
export ALDO_PROXY="socks5://127.0.0.1:9050"
```

SoulSeek cannot be proxied. It owns its own socket and its distributed search
needs inbound reachability that a proxy cannot provide, so a proxy here is
metadata-only by construction rather than by omission.
