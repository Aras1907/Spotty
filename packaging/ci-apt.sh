#!/bin/sh
# Install apt packages on a GitHub runner. The runner's DNS sometimes stops
# resolving the Ubuntu mirrors (v0.6.20 failed twice on it), so switch to
# public DNS when they don't resolve, and retry the whole install.
# Usage: sh packaging/ci-apt.sh pkg...
set -u
for host in archive.ubuntu.com security.ubuntu.com ports.ubuntu.com azure.archive.ubuntu.com; do
  if ! getent hosts "$host" >/dev/null; then
    echo "::warning::$host does not resolve; switching to public DNS"
    sudo mkdir -p /etc/systemd/resolved.conf.d
    printf '[Resolve]\nDNS=1.1.1.1 8.8.8.8 9.9.9.9\nDomains=~.\n' |
      sudo tee /etc/systemd/resolved.conf.d/ci-dns.conf >/dev/null
    sudo systemctl restart systemd-resolved
    sleep 2
    break
  fi
done
for try in 1 2 3; do
  sudo apt-get -o Acquire::Retries=5 update -qq &&
    sudo apt-get -o Acquire::Retries=5 install -y -qq "$@" && exit 0
  echo "::warning::apt attempt $try failed; retrying in 30s"
  sleep 30
done
exit 1
