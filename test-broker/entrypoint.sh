#!/bin/sh
set -eu

generated=/mosquitto/generated
mkdir -p "$generated"

openssl req -x509 -newkey rsa:2048 -nodes -days 2 \
  -subj "/CN=MQUI test CA" \
  -keyout "$generated/ca.key" -out "$generated/ca.crt"
openssl req -newkey rsa:2048 -nodes \
  -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
  -keyout "$generated/server.key" -out "$generated/server.csr"
printf '%s\n' \
  'authorityKeyIdentifier=keyid,issuer' \
  'basicConstraints=CA:FALSE' \
  'keyUsage=digitalSignature,keyEncipherment' \
  'extendedKeyUsage=serverAuth' \
  'subjectAltName=DNS:localhost,IP:127.0.0.1' > "$generated/server.ext"
openssl x509 -req -days 2 -sha256 \
  -in "$generated/server.csr" \
  -CA "$generated/ca.crt" -CAkey "$generated/ca.key" -CAcreateserial \
  -extfile "$generated/server.ext" -out "$generated/server.crt"

mosquitto_passwd -b -c "$generated/passwords" mqui-test correct-password
chmod 644 "$generated/ca.crt" "$generated/server.crt" "$generated/server.key" "$generated/passwords"
exec mosquitto -c /mosquitto/config/mosquitto.conf
