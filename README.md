# catchup

Fetch every git repo you have open in [cmux](https://cmux.com) and browse what's new.

catchup lists the default branch and the checked-out branches of each repo,
with the commits the fetch brought in and what's waiting to be pulled.
Open a branch's new commits in [glog](https://github.com/yairchu/glog)
(or `git log -p` when glog isn't installed), and pull without leaving the list.

## Install

```sh
cargo install --git https://github.com/yairchu/catchup
```

## Usage

```sh
catchup             # the repos open in cmux
catchup DIR...      # these repos instead
catchup --print     # print a summary instead of the TUI
catchup --no-fetch  # skip fetching, only show what can be pulled
```

| Key               | Action                                       |
|-------------------|----------------------------------------------|
| `↑` `↓` / `j` `k` | select                                       |
| `⏎` / `o` / click | open the current detail view                  |
| `w`               | open it in a split in the repo's workspace   |
| `d`               | toggle range-diff and fetched commits        |
| `[` / `]`         | scroll the detail pane                       |
| `p` / `P`         | pull the selected branch / all branches      |
| `r` / Refresh     | refresh all listed repos                     |
| `q`               | quit                                         |

Refresh runs in the background and shows newly fetched commits and anything
still waiting to be pulled. If you started Catchup without directories, refresh
also adds repositories opened in cmux since the last scan. With `--no-fetch`,
it rescans without fetching.
Click Refresh in the footer or press `r`; wait for a repo's refresh to finish
before pulling it.

When a local branch and its upstream diverge, the detail pane shows a
range-diff summary from local to upstream, even when the latest fetch brought
in no commits. Press `d` to switch to the fetched commits. Catchup shows the
fetched commits automatically if either side contains a merge in the
diverged history or Git cannot produce the comparison.
Opening the range-diff shows the full comparison in glog, or in Git's pager
if glog is unavailable or lacks range-diff support. Press Escape to return
to Catchup when your glog supports `--exit-on-esc` for range-diff.

## License

MIT
