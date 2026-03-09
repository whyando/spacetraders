#!/bin/bash

trap "exit" INT TERM ERR
trap "kill 0" EXIT

kubectl --context=jpa-dev -n postgres port-forward svc/postgresql 5432:5432 &

wait
