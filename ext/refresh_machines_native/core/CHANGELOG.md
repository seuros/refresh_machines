# Changelog

## [0.5.0](https://github.com/seuros/refresh_machines/compare/refresh-machines-v0.4.0...refresh-machines-v0.5.0) (2026-11-13)


### ⚠ BREAKING CHANGES

* see CONVENTIONS: one exhaustive Refusal per crate, Result from every mutator, honoured idempotent repeats only when provable, Duration time, Ruby mirrors the core. No compatibility aliases. Requires coalesce-machines >= 0.5. Ruby times are Integer nanoseconds; the Refused alias is gone.

### Features

* Duration instants and spans, Wait names its flight, forget answers Forgetting, Ruby mirrors Decision and Revalidation ([f275b16](https://github.com/seuros/refresh_machines/commit/f275b1663bed9b658d6477a5542886c230946b90))

## [0.4.0](https://github.com/seuros/refresh_machines/compare/refresh-machines-v0.3.0...refresh-machines-v0.4.0) (2026-11-13)


### ⚠ BREAKING CHANGES

* Refresher::drain takes &mut [Notification] and sweep takes &mut [u64] instead of Option slices; RefreshMachines::Notification gains flight_id as its second field. Refused is kept as a deprecated alias of Refusal. Requires coalesce-machines >= 0.4.

### Features

* map leases to flights, carry flight_id in notifications, plain output buffers ([0564a61](https://github.com/seuros/refresh_machines/commit/0564a61d8588cbe390248eab537e67bce7b1a45e))

## [0.3.0](https://github.com/seuros/refresh_machines/compare/refresh-machines-v0.2.0...refresh-machines-v0.3.0) (2026-11-13)


### Features

* scope leases and waiter ids so separate refreshers never share one ([66940fb](https://github.com/seuros/refresh_machines/commit/66940fbca88d4e28e6045ef3ec21c8daf0c1325d))

## [0.2.0](https://github.com/seuros/refresh_machines/compare/refresh-machines-v0.1.0...refresh-machines-v0.2.0) (2026-11-12)


### Features

* initial extraction ([38f59b3](https://github.com/seuros/refresh_machines/commit/38f59b36131d87ca31a0ad348a9052a62045541d))
