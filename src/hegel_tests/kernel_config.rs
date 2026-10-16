use hegel::TestCase;
use hegel::generators as gs;

use crate::InitFlags;
use crate::KernelConfig;
use crate::SessionACL;
use crate::ll::Version;

fn assert_config_invariants(config: &KernelConfig) {
    assert!(config.congestion_threshold() >= 1);
    assert!(config.congestion_threshold() <= config.max_background);
    assert!(config.max_pages() >= 1);
    assert_eq!(
        u32::from(config.max_pages()),
        (config.max_write.max(config.max_readahead).saturating_sub(1) / page_size::get() as u32)
            + 1
    );
}

#[hegel::test]
fn kernel_config_setters_report_previous_or_nearest_values(tc: TestCase) {
    let mut config = KernelConfig::new(
        InitFlags::empty(),
        1 << 20,
        Version(7, 44),
        false,
        SessionACL::All,
    );
    let background = tc.draw(gs::integers::<u16>());
    let previous_background = config.max_background;
    match config.set_max_background(background) {
        Ok(previous) => assert_eq!(previous, previous_background),
        Err(nearest) => assert_eq!(config.set_max_background(nearest), Ok(previous_background)),
    }
    let congestion = tc.draw(gs::integers::<u16>());
    let previous = config.congestion_threshold();
    match config.set_congestion_threshold(congestion) {
        Ok(old) => assert_eq!(old, previous),
        Err(nearest) => assert_eq!(config.set_congestion_threshold(nearest), Ok(previous)),
    }
    assert_config_invariants(&config);
}

#[hegel::test]
fn kernel_config_background_sequence_preserves_invariants(tc: TestCase) {
    let mut config = KernelConfig::new(
        InitFlags::empty(),
        1 << 20,
        Version(7, 44),
        false,
        SessionACL::All,
    );
    let values = tc.draw(gs::vecs(gs::integers::<u16>()).max_size(40));
    for value in values {
        if tc.draw(gs::booleans()) {
            let _ = config.set_max_background(value);
        } else {
            let _ = config.set_congestion_threshold(value);
        }
        assert_config_invariants(&config);
    }
}
