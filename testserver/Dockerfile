# A throwaway Linux SSH server for the live tests in src/sshconn.rs.
# It deliberately contains root-only files so the sudo path can be exercised.
#
#   docker build -t sshman-test testserver
#   docker run -d --name sshman-test -p 2222:22 sshman-test
#
# See testserver/run-live-tests.sh for running the tests against it.
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server sudo \
 && rm -rf /var/lib/apt/lists/*

RUN mkdir /var/run/sshd \
 && useradd -m -s /bin/bash tester \
 && echo 'tester:testpass' | chpasswd \
 && adduser tester sudo \
 && echo 'root:rootpass' | chpasswd

# Files the login user cannot read, so tests can prove sudo mode reaches them.
RUN mkdir -p /root/secretdir \
 && echo 'top secret contents' > /root/secretdir/secret.txt \
 && echo 'db_password=hunter2' > /etc/app-private.conf \
 && chmod 600 /etc/app-private.conf \
 && echo 'a file with spaces' > '/root/two words.txt'

RUN sed -i 's/#PasswordAuthentication yes/PasswordAuthentication yes/' /etc/ssh/sshd_config

EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
