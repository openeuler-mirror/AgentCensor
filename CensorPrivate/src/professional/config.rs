use crate::error::{Error, Result};
use std::{collections::BTreeSet, fmt, str::FromStr};

/// Execution backend and visible device ordinals, independent of any harness.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum NerDevice {
    #[default]
    Cpu,
    Cuda(Vec<i32>),
    Cann(Vec<i32>),
}
impl NerDevice {
    pub(super) fn validate(&self) -> Result<()> {
        if let Self::Cuda(ids) | Self::Cann(ids) = self {
            if ids.is_empty()
                || ids.len() > 64
                || ids.iter().any(|id| *id < 0)
                || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
            {
                return Err(Error::NerConfigInvalid);
            }
        }
        Ok(())
    }
}
impl FromStr for NerDevice {
    type Err = &'static str;
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        const INVALID: &str =
            "expected cpu, cuda[:ids], or cann[:ids] (unique nonnegative device IDs)";
        if value == "cpu" {
            return Ok(Self::Cpu);
        }
        let (backend, devices) = match value.split_once(':') {
            None => (value, "0"),
            Some(pair) => pair,
        };
        if !matches!(backend, "cuda" | "cann") {
            return Err(INVALID);
        }
        let ids = devices
            .split(',')
            .map(|id| {
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(INVALID);
                }
                id.parse::<i32>().map_err(|_| INVALID)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let device = if backend == "cuda" {
            Self::Cuda(ids)
        } else {
            Self::Cann(ids)
        };
        device.validate().map_err(|_| INVALID)?;
        Ok(device)
    }
}
impl fmt::Display for NerDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cpu => f.write_str("cpu"),
            Self::Cuda(ids) | Self::Cann(ids) => {
                f.write_str(if matches!(self, Self::Cuda(_)) {
                    "cuda:"
                } else {
                    "cann:"
                })?;
                for (index, id) in ids.iter().enumerate() {
                    if index > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{id}")?;
                }
                Ok(())
            }
        }
    }
}

pub fn configured_ner_device(explicit: Option<NerDevice>) -> Result<NerDevice> {
    let device = match explicit {
        Some(device) => device,
        None => match std::env::var("CP_NER_DEVICE") {
            Ok(value) => value.parse().map_err(|_| Error::NerConfigInvalid)?,
            Err(std::env::VarError::NotPresent) => NerDevice::Cpu,
            Err(_) => return Err(Error::NerConfigInvalid),
        },
    };
    device.validate()?;
    Ok(device)
}
pub fn configured_ner_batch_size(explicit: Option<u16>) -> Result<usize> {
    let size = match explicit {
        Some(size) => usize::from(size),
        None => match std::env::var("CP_NER_BATCH_SIZE") {
            Ok(value) => value.parse().map_err(|_| Error::NerConfigInvalid)?,
            Err(std::env::VarError::NotPresent) => 1,
            Err(_) => return Err(Error::NerConfigInvalid),
        },
    };
    if !(1..=256).contains(&size) {
        return Err(Error::NerConfigInvalid);
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_selection_normalizes_cuda_and_rejects_ambiguous_lists() {
        for (input, expected) in [
            ("cpu", "cpu"),
            ("cuda", "cuda:0"),
            ("cann", "cann:0"),
            ("cann:0,1", "cann:0,1"),
            ("cuda:0,1,2,3", "cuda:0,1,2,3"),
            ("cuda:2,0", "cuda:2,0"),
        ] {
            assert_eq!(input.parse::<NerDevice>().unwrap().to_string(), expected);
        }
        for input in [
            "",
            "auto",
            "rocm",
            "cann:",
            "cann:0,0",
            "cann:-1",
            "cpu:0",
            "cuda:",
            "cuda:0,",
            "cuda:-1",
            "cuda:+1",
            "cuda:0,0",
            "cuda:0,00",
            "cuda:0, 1",
            "cuda:2147483648",
        ] {
            assert!(input.parse::<NerDevice>().is_err(), "{input}");
        }
        assert!(configured_ner_device(Some(NerDevice::Cuda(vec![]))).is_err());
        assert!(configured_ner_device(Some(NerDevice::Cuda(vec![-1]))).is_err());
        assert_eq!(configured_ner_batch_size(Some(4)).unwrap(), 4);
        assert!(configured_ner_batch_size(Some(0)).is_err());
        assert!(configured_ner_batch_size(Some(257)).is_err());
    }
}
