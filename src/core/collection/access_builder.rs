use crate::core::collection::Access;

/// Builder for [`Access`]. Created via [`Access::builder`].
pub struct AccessBuilder {
    read: Option<String>,
    create: Option<String>,
    update: Option<String>,
    delete: Option<String>,
    trash: Option<String>,
}

impl AccessBuilder {
    pub(crate) fn new() -> Self {
        Self {
            read: None,
            create: None,
            update: None,
            delete: None,
            trash: None,
        }
    }

    pub fn read(mut self, read: Option<String>) -> Self {
        self.read = read;
        self
    }

    pub fn create(mut self, create: Option<String>) -> Self {
        self.create = create;
        self
    }

    pub fn update(mut self, update: Option<String>) -> Self {
        self.update = update;
        self
    }

    pub fn delete(mut self, delete: Option<String>) -> Self {
        self.delete = delete;
        self
    }

    pub fn trash(mut self, trash: Option<String>) -> Self {
        self.trash = trash;
        self
    }

    pub fn build(self) -> Access {
        Access {
            read: self.read,
            create: self.create,
            update: self.update,
            delete: self.delete,
            trash: self.trash,
        }
    }
}
