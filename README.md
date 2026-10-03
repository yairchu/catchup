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
| `⏎` / `o` / click | show the new commits                         |
| `w`               | show them in a split in the repo's workspace |
| `p` / `P`         | pull the selected branch / all branches      |
| `q`               | quit                                         |

## License

MIT
