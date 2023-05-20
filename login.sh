#!/bin/bash
# Script to help build the bootstrap secret for bitwarden-secrets-operator
set -oeu pipefail

BW_NAMESPACE="bitwarden-secrets-operator"
BW_CLIENTID=""
BW_CLIENTSECRET=""
BW_PASSWORD=""

read -s -p "Enter your Bitwarden client id: " BW_CLIENTID
echo
read -s -p "Enter your Bitwarden client secret: " BW_CLIENTSECRET
echo
read -s -p "Enter your Bitwarden password: " BW_PASSWORD
echo

echo "Creating secret with provided credentials..."
kubectl create namespace $BW_NAMESPACE || /bin/true
kubectl create secret generic bitwarden-credentials \
    --from-literal=BW_CLIENTID=$BW_CLIENTID \
    --from-literal=BW_CLIENTSECRET=$BW_CLIENTSECRET \
    --from-literal=BW_PASSWORD=$BW_PASSWORD \
    -n $BW_NAMESPACE
