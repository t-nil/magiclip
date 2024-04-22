use clap::Parser;

use crate::ffmpeg::EncodingProfile;

#[derive(Clone, Debug, PartialEq, Eq, Parser)]
#[command(version, about, long_about = None)]
pub struct Args {
    #[arg(short, long, default_value = "av1")]
    pub profile: EncodingProfile,
}
