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

# mount
$ ghfs https://github.com/<owner>/<repo> /path/to/directory

# unmount
$ fusermount -u /path/to/directory
```
You can see help for parameters with `--help`.
