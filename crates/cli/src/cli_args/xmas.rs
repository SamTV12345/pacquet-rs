use clap::Args;

#[derive(Debug, Args, Default)]
pub struct XmasArgs;

impl XmasArgs {
    pub fn run(self) -> miette::Result<()> {
        let tree = [
            "            *",
            "           /o\\",
            "          /o o\\",
            "         /o o o\\",
            "        /o o o o\\",
            "       /o o o o o\\",
            "      /o o o o o o\\",
            "     /o o o o o o o\\",
            "    /o o o o o o o o\\",
            "   /o o o o o o o o o\\",
            "          |||",
            "          |||",
        ];
        for line in tree {
            println!("{line}");
        }
        println!("\nHappy Holidays from pacquet!");
        Ok(())
    }
}
