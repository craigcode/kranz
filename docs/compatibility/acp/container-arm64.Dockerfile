FROM python@sha256:ae8c3b6bb3d02ffffa2bee0bed8c7c99b9478bd042ff4d79a5bbd6eb10398864
COPY node-v22.23.1-linux-arm64.tar.xz /tmp/node.tar.xz
RUN python3 -c 'import tarfile; tarfile.open("/tmp/node.tar.xz").extractall("/opt/node", filter="data")' && ln -s /opt/node/node-v22.23.1-linux-arm64/bin/node /usr/local/bin/node && ln -s /opt/node/node-v22.23.1-linux-arm64/bin/npm /usr/local/bin/npm && rm /tmp/node.tar.xz
WORKDIR /opt/acp
COPY package.json package-lock.json ./
RUN touch /tmp/npm-user /tmp/npm-global && npm ci --ignore-scripts --no-audit --no-fund --userconfig /tmp/npm-user --globalconfig /tmp/npm-global && npm cache clean --force
