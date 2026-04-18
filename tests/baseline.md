OS: Arch Linux x86_64
Kernel: Linux 6.18.9-arch1-2
CPU: Intel Ultra 7 165H (22) @ 4.700GHz [75.0°on]
Memory: 96191MiB


# Sqlite | pool_max_size: 64, vm_pool_size: 16

Scenario              Conc      Req/s        p50        p95        p99   Errors
──────────────────── ───── ────────── ────────── ────────── ────────── ────────
describe                 1     3436.1     0.18ms     0.49ms     0.71ms        0
describe                10    14140.2     0.63ms     0.98ms      1.4ms    0.00%
describe                50    20868.1      2.2ms      3.7ms      4.8ms    0.02%
count                    1     2568.9     0.32ms     0.49ms     0.76ms    0.00%
count                   10    12865.3     0.69ms      1.0ms      1.3ms    0.00%
count                   50    21409.8      2.2ms      3.5ms      4.4ms    0.02%
find                     1      521.1      1.7ms      2.7ms      3.5ms        0
find                    10     2959.5      3.1ms      5.0ms      5.9ms    0.03%
find                    50     1841.7     29.9ms     36.0ms     39.0ms    0.22%
find_where               1      504.1      1.8ms      2.7ms      3.6ms    0.02%
find_where              10     2931.0      3.1ms      5.1ms      6.2ms    0.03%
find_where              50     1872.4     29.4ms     35.8ms     38.8ms    0.27%
find_by_id               1      917.3     0.97ms      1.6ms      1.9ms        0
find_by_id              10     6249.0      1.5ms      2.2ms      2.8ms    0.01%
find_by_id              50     9589.6      5.0ms      7.6ms      9.0ms    0.05%
find_deep                1       83.7     11.2ms     15.6ms     19.5ms        0
find_deep               10      509.2     18.6ms     27.9ms     31.3ms    0.16%
find_deep               50      700.0     70.0ms      101ms      117ms    0.71%
find_deep5               1       77.3     12.3ms     16.0ms     20.8ms    0.13%
find_deep5              10      469.0     20.2ms     29.9ms     33.3ms    0.13%
find_deep5              50      680.1     72.3ms      105ms      125ms    0.72%
create                   1      386.9      2.1ms      3.4ms     15.2ms    0.03%
create                  10      277.7      7.1ms      135ms      636ms    0.36%
create                  50      252.7      101ms      553ms     2144ms    1.98%
update                   1      655.7      1.3ms      1.9ms      7.0ms        0
update                  10      640.7      3.7ms     57.8ms      182ms    0.14%
update                  50      668.1     40.1ms      163ms      968ms    0.73%


# Postgres | pool_max_size: 64, vm_pool_size: 16

Scenario              Conc      Req/s        p50        p95        p99   Errors
──────────────────── ───── ────────── ────────── ────────── ────────── ────────
describe                 1     3503.8     0.17ms     0.48ms     0.69ms    0.00%
describe                10    13895.2     0.64ms     0.99ms      1.5ms    0.00%
describe                50    21041.1      2.2ms      3.7ms      4.7ms    0.01%
count                    1      893.2     0.96ms      1.5ms      1.9ms    0.01%
count                   10     6166.6      1.5ms      2.0ms      2.6ms    0.01%
count                   50     8504.4      5.8ms      7.8ms      8.9ms    0.06%
find                     1      133.2      6.6ms     11.3ms     15.6ms        0
find                    10      807.9     12.1ms     14.9ms     16.8ms    0.11%
find                    50     1117.8     44.4ms     51.6ms     55.4ms    0.45%
find_where               1      131.0      6.8ms     11.3ms     14.0ms    0.08%
find_where              10      811.9     12.0ms     14.9ms     17.2ms    0.12%
find_where              50     1113.0     44.5ms     52.3ms     56.4ms    0.31%
find_by_id               1      370.1      2.6ms      3.8ms      5.6ms    0.03%
find_by_id              10     2775.1      3.4ms      4.4ms      5.2ms    0.04%
find_by_id              50     4193.9     11.7ms     15.4ms     17.5ms    0.10%
find_deep                1       46.8     20.2ms     28.5ms     34.5ms    0.21%
find_deep               10      295.0     33.2ms     42.0ms     46.4ms    0.27%
find_deep               50      400.6      124ms      147ms      192ms    1.22%
find_deep5               1       44.6     21.4ms     29.7ms     35.7ms        0
find_deep5              10      285.9     34.2ms     43.2ms     47.6ms    0.31%
find_deep5              50      387.4      129ms      151ms      161ms    1.19%
create                   1      108.6      8.3ms     19.4ms     28.7ms    0.09%
create                  10      677.2     14.1ms     19.9ms     25.2ms    0.15%
create                  50      602.7     72.8ms      147ms      201ms    0.83%
update                   1      145.1      6.1ms     10.3ms     12.4ms    0.07%
update                  10      156.3     38.8ms      194ms      356ms    0.58%
update                  50      122.2      348ms      845ms     1231ms    4.09%
