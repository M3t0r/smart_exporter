# smart_exporter

Simple test execution (taking S.M.A.R.T. stats from test data):

```bash
cargo run -- --invoker=file --file-invoker-path tests/simple/ -b 127.0.0.1:9940 &
curl -sv localhost:9940/metrics
```

## Prior Art

 - https://github.com/prometheus-community/node-exporter-textfile-collector-scripts/blob/master/smartmon.sh
 - https://github.com/prometheus-community/node-exporter-textfile-collector-scripts/blob/master/smartmon.py
 - https://github.com/matusnovak/prometheus-smartctl
 - https://github.com/prometheus-community/smartctl_exporter
 - https://github.com/cloudandheat/prometheus_smart_exporter

