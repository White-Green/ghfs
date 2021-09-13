# ghfs
GitHubリポジトリをファイルにマウントしてアクセスできるよ
APIで非同期にアクセスするから、でかいリポジトリをcloneしたくないときに使えるよ

# Require
- OS:Linux(macでも動くかも(Dokanyにも対応したいね))
- libfuse-dev and libssl-dev are required in Ubuntu.

# Usage
```
# install
$ cargo install --git https://github.com/White-Green/ghfs --branch main

# register Personal Access Token
$ ghfs token set

# remove Personal Access Token registered
$ ghfs token remove

# mount (Press Ctrl+C to unmount)
$ ghfs mount https://github.com/<owner>/<repo> /path/to/directory

# unmount (Required only if the ghfs process is killed)
$ fusermount -u /path/to/directory
```
You can see help for parameters with `--help`.
