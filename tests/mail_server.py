"""Capture verification messages from an isolated encrypted mail server."""

import argparse
import socketserver
import ssl
import time
import uuid
from pathlib import Path


class MailHandler(socketserver.StreamRequestHandler):
    def handle(self):
        self.wfile.write(b"220 mail.test ready\r\n")
        encrypted = False
        while True:
            line = self.rfile.readline()
            if not line:
                return
            command = line.decode("utf-8", errors="replace").strip().upper()
            if command.startswith("EHLO") or command.startswith("HELO"):
                self.wfile.write(b"250-mail.test\r\n")
                if not encrypted:
                    self.wfile.write(b"250-STARTTLS\r\n")
                self.wfile.write(b"250 8BITMIME\r\n")
            elif command == "STARTTLS" and not encrypted:
                self.wfile.write(b"220 Ready to start encryption\r\n")
                self.wfile.flush()
                self.connection = self.server.context.wrap_socket(
                    self.connection, server_side=True
                )
                self.rfile = self.connection.makefile("rb", self.rbufsize)
                self.wfile = self.connection.makefile("wb", self.wbufsize)
                encrypted = True
            elif command.startswith("MAIL FROM:") or command.startswith("RCPT TO:"):
                self.wfile.write(b"250 Accepted\r\n")
            elif command == "DATA":
                self.wfile.write(b"354 Send message\r\n")
                content = bytearray()
                while True:
                    data_line = self.rfile.readline()
                    if not data_line or data_line in (b".\r\n", b".\n"):
                        break
                    if data_line.startswith(b".."):
                        data_line = data_line[1:]
                    content.extend(data_line)
                message_path = self.server.message_directory / (
                    f"{time.time_ns()}_{uuid.uuid4().hex}.eml"
                )
                message_path.write_bytes(content)
                self.wfile.write(b"250 Message accepted\r\n")
            elif command == "QUIT":
                self.wfile.write(b"221 Goodbye\r\n")
                return
            else:
                self.wfile.write(b"502 Unsupported command\r\n")
            self.wfile.flush()


class MailServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", required=True)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--certificate", required=True)
    parser.add_argument("--key", required=True)
    arguments = parser.parse_args()
    arguments.directory.mkdir(parents=True, exist_ok=True)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(arguments.certificate, arguments.key)
    with MailServer((arguments.host, arguments.port), MailHandler) as server:
        server.context = context
        server.message_directory = arguments.directory
        server.serve_forever()


if __name__ == "__main__":
    main()
