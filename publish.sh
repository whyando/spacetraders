#!/bin/bash
set -e

# Configuration
REGISTRY="registry.jpa-dev.whyando.com"
IMAGE_NAME="whyando/spacetraders"
VERSION=$(grep '^version = ' Cargo.toml | cut -d'"' -f2)

echo "Building version: $VERSION"

# Build the image
docker buildx build -t ${REGISTRY}/${IMAGE_NAME}:${VERSION} .
# Push the image
docker push ${REGISTRY}/${IMAGE_NAME}:${VERSION}

echo "Successfully built and pushed ${REGISTRY}/${IMAGE_NAME}:${VERSION}"
