use process_wrap::tokio::{CommandWrap, JobObject, KillOnDrop};

pub(crate) fn wrap(command: &mut CommandWrap) {
    command.wrap(JobObject);
    command.wrap(KillOnDrop);
}
