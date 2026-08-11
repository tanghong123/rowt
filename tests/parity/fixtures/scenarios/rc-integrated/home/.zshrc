# a user's rc that already has rowt shell integration in it, so
# `shell-init --install` must decline rather than append a second copy.
export PATH="/usr/local/bin:$PATH"

# rowt shell integration — aliases + tab-completion (rowt shell-init --install)
eval "$(rowt shell-init)"
export EDITOR=vim
