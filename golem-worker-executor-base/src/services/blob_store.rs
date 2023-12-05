use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_s3 as s3;
use golem_common::model::AccountId;
use s3::config::Region;
use tokio::io::AsyncReadExt;

use crate::services::golem_config::{BlobStoreServiceConfig, BlobStoreServiceS3Config};

/// Interface for storing blobs in a persistent storage.
#[async_trait]
pub trait BlobStoreService {
    async fn clear(&self, account_id: AccountId, container_name: String) -> anyhow::Result<()>;

    async fn container_exists(
        &self,
        account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<bool>;

    async fn copy_object(
        &self,
        account_id: AccountId,
        source_container_name: String,
        source_object_name: String,
        destination_container_name: String,
        destination_object_name: String,
    ) -> anyhow::Result<()>;

    async fn create_container(
        &self,
        account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<u64>;

    async fn delete_container(
        &self,
        account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<()>;

    async fn delete_object(
        &self,
        account_id: AccountId,
        container_name: String,
        object_name: String,
    ) -> anyhow::Result<()>;

    async fn delete_objects(
        &self,
        account_id: AccountId,
        container_name: String,
        object_names: Vec<String>,
    ) -> anyhow::Result<()>;

    async fn get_container(
        &self,
        account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<Option<u64>>;

    async fn get_data(
        &self,
        account_id: AccountId,
        container_name: String,
        object_name: String,
        start: u64,
        end: u64,
    ) -> anyhow::Result<Vec<u8>>;

    async fn has_object(
        &self,
        account_id: AccountId,
        container_name: String,
        object_name: String,
    ) -> anyhow::Result<bool>;

    async fn list_objects(
        &self,
        account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<Vec<String>>;

    async fn move_object(
        &self,
        account_id: AccountId,
        source_container_name: String,
        source_object_name: String,
        destination_container_name: String,
        destination_object_name: String,
    ) -> anyhow::Result<()>;

    async fn object_info(
        &self,
        account_id: AccountId,
        container_name: String,
        object_name: String,
    ) -> anyhow::Result<ObjectMetadata>;

    async fn write_data(
        &self,
        account_id: AccountId,
        container_name: String,
        object_name: String,
        data: Vec<u8>,
    ) -> anyhow::Result<()>;
}

pub async fn configured(
    config: &BlobStoreServiceConfig,
) -> Arc<dyn BlobStoreService + Send + Sync> {
    match config {
        BlobStoreServiceConfig::S3(config) => {
            let region = config.region.clone();
            let sdk_config = aws_config::defaults(BehaviorVersion::v2023_11_09())
                .region(Region::new(region))
                .load()
                .await;
            Arc::new(BlobStoreServiceS3 {
                config: config.clone(),
                client: s3::Client::new(&sdk_config),
            })
        }
        BlobStoreServiceConfig::InMemory(_) => Arc::new(BlobStoreServiceInMemory::new()),
    }
}

pub struct BlobStoreServiceS3 {
    config: BlobStoreServiceS3Config,
    client: s3::Client,
}

impl BlobStoreServiceS3 {
    fn bucket_name(&self, account_id: &AccountId, container_name: &String) -> String {
        format!(
            "instance:blobstore:{}:{}:{}",
            self.config.bucket_prefix, account_id, container_name
        )
    }
}

#[async_trait]
impl BlobStoreService for BlobStoreServiceS3 {
    async fn clear(&self, account_id: AccountId, container_name: String) -> anyhow::Result<()> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        let mut continuation_token = None;
        loop {
            let list_objects_v2_output = self
                .client
                .list_objects_v2()
                .bucket(&bucket_name)
                .set_continuation_token(continuation_token)
                .send()
                .await?;
            if let Some(contents) = list_objects_v2_output.contents {
                for object in contents {
                    self.client
                        .delete_object()
                        .bucket(&bucket_name)
                        .key(object.key.unwrap())
                        .send()
                        .await?;
                }
            }
            if list_objects_v2_output.next_continuation_token.is_none() {
                break;
            }
            continuation_token = list_objects_v2_output.next_continuation_token;
        }
        Ok(())
    }

    async fn container_exists(
        &self,
        account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<bool> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        let list_buckets_output = self.client.list_buckets().send().await?;
        if let Some(buckets) = list_buckets_output.buckets {
            for bucket in buckets {
                if bucket.name.unwrap() == bucket_name {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    async fn copy_object(
        &self,
        account_id: AccountId,
        source_container_name: String,
        source_object_name: String,
        destination_container_name: String,
        destination_object_name: String,
    ) -> anyhow::Result<()> {
        let source_bucket_name = self.bucket_name(&account_id, &source_container_name);
        let destination_bucket_name = self.bucket_name(&account_id, &destination_container_name);
        self.client
            .copy_object()
            .bucket(&destination_bucket_name)
            .copy_source(format!("{}/{}", source_bucket_name, source_object_name))
            .key(destination_object_name)
            .send()
            .await?;
        Ok(())
    }

    async fn create_container(
        &self,
        account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<u64> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        let location_constraint =
            s3::types::BucketLocationConstraint::from(self.config.region.as_str());
        let create_bucket_configuration = s3::types::CreateBucketConfiguration::builder()
            .location_constraint(location_constraint)
            .build();
        self.client
            .create_bucket()
            .bucket(&bucket_name)
            .create_bucket_configuration(create_bucket_configuration)
            .send()
            .await?;
        let list_buckets_output = self.client.list_buckets().send().await?;
        if let Some(buckets) = list_buckets_output.buckets {
            for bucket in buckets {
                if bucket.name.unwrap() == bucket_name {
                    return Ok(bucket.creation_date.unwrap().to_millis()? as u64);
                }
            }
        }
        anyhow::bail!("Failed to create container");
    }

    async fn delete_container(
        &self,
        account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<()> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        self.client
            .delete_bucket()
            .bucket(&bucket_name)
            .send()
            .await?;
        Ok(())
    }

    async fn delete_object(
        &self,
        account_id: AccountId,
        container_name: String,
        object_name: String,
    ) -> anyhow::Result<()> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        self.client
            .delete_object()
            .bucket(&bucket_name)
            .key(object_name)
            .send()
            .await?;
        Ok(())
    }

    async fn delete_objects(
        &self,
        account_id: AccountId,
        container_name: String,
        object_names: Vec<String>,
    ) -> anyhow::Result<()> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        let mut object_identifiers = Vec::new();
        for object_name in object_names {
            object_identifiers.push(
                s3::types::ObjectIdentifier::builder()
                    .key(object_name)
                    .build()?,
            );
        }
        let delete = s3::types::Delete::builder()
            .set_objects(Some(object_identifiers))
            .build()?;
        self.client
            .delete_objects()
            .bucket(&bucket_name)
            .delete(delete)
            .send()
            .await?;
        Ok(())
    }

    async fn get_container(
        &self,
        account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<Option<u64>> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        let list_buckets_output = self.client.list_buckets().send().await?;
        if let Some(buckets) = list_buckets_output.buckets {
            for bucket in buckets {
                if bucket.name.unwrap() == bucket_name {
                    return Ok(Some(bucket.creation_date.unwrap().to_millis()? as u64));
                }
            }
        }
        Ok(None)
    }

    async fn get_data(
        &self,
        account_id: AccountId,
        container_name: String,
        object_name: String,
        start: u64,
        end: u64,
    ) -> anyhow::Result<Vec<u8>> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        let get_object_output = self
            .client
            .get_object()
            .bucket(&bucket_name)
            .key(object_name)
            .range(format!("bytes={}-{}", start, end))
            .send()
            .await?;
        let body = get_object_output.body;
        let mut data = Vec::new();
        body.into_async_read().read_to_end(&mut data).await?;
        Ok(data)
    }

    async fn has_object(
        &self,
        account_id: AccountId,
        container_name: String,
        object_name: String,
    ) -> anyhow::Result<bool> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        let list_objects_v2_output = self
            .client
            .list_objects_v2()
            .bucket(&bucket_name)
            .prefix(object_name.clone())
            .send()
            .await?;
        if let Some(contents) = list_objects_v2_output.contents {
            for object in contents {
                if object.key.unwrap() == object_name {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    async fn list_objects(
        &self,
        account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<Vec<String>> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        let mut continuation_token = None;
        let mut object_names = Vec::new();
        loop {
            let list_objects_v2_output = self
                .client
                .list_objects_v2()
                .bucket(&bucket_name)
                .set_continuation_token(continuation_token)
                .send()
                .await?;
            if let Some(contents) = list_objects_v2_output.contents {
                for object in contents {
                    object_names.push(object.key.unwrap());
                }
            }
            if list_objects_v2_output.next_continuation_token.is_none() {
                break;
            }
            continuation_token = list_objects_v2_output.next_continuation_token;
        }
        Ok(object_names)
    }

    async fn move_object(
        &self,
        account_id: AccountId,
        source_container_name: String,
        source_object_name: String,
        destination_container_name: String,
        destination_object_name: String,
    ) -> anyhow::Result<()> {
        let source_bucket_name = self.bucket_name(&account_id, &source_container_name);
        let destination_bucket_name = self.bucket_name(&account_id, &destination_container_name);
        self.client
            .copy_object()
            .bucket(&destination_bucket_name)
            .copy_source(format!("{}/{}", source_bucket_name, source_object_name))
            .key(destination_object_name)
            .send()
            .await?;
        self.client
            .delete_object()
            .bucket(&source_bucket_name)
            .key(source_object_name)
            .send()
            .await?;
        Ok(())
    }

    async fn object_info(
        &self,
        account_id: AccountId,
        container_name: String,
        object_name: String,
    ) -> anyhow::Result<ObjectMetadata> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        let list_objects_v2_output = self
            .client
            .list_objects_v2()
            .bucket(&bucket_name)
            .prefix(object_name.clone())
            .send()
            .await?;
        if let Some(contents) = list_objects_v2_output.contents {
            for object in contents {
                if object.key.clone().unwrap() == object_name {
                    return Ok(ObjectMetadata {
                        name: object.key.unwrap(),
                        container: container_name,
                        created_at: object.last_modified.unwrap().to_millis()? as u64,
                        size: object.size.unwrap_or(0) as u64,
                    });
                }
            }
        }
        anyhow::bail!("Object does not exist");
    }

    async fn write_data(
        &self,
        account_id: AccountId,
        container_name: String,
        object_name: String,
        data: Vec<u8>,
    ) -> anyhow::Result<()> {
        let bucket_name = self.bucket_name(&account_id, &container_name);
        self.client
            .put_object()
            .bucket(&bucket_name)
            .key(object_name)
            .body(data.into())
            .send()
            .await?;
        Ok(())
    }
}

type Containers = HashMap<String, (u64, Objects)>;
type Objects = HashMap<String, (u64, Vec<u8>)>;

pub struct BlobStoreServiceInMemory {
    pub containers: Arc<RwLock<Containers>>,
}

impl Default for BlobStoreServiceInMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl BlobStoreServiceInMemory {
    pub fn new() -> Self {
        Self {
            containers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl BlobStoreService for BlobStoreServiceInMemory {
    async fn clear(&self, _account_id: AccountId, container_name: String) -> anyhow::Result<()> {
        let mut containers = self.containers.write().unwrap();
        if !containers.contains_key(&container_name) {
            anyhow::bail!("Container does not exist");
        }
        let (_, objects) = containers.get_mut(&container_name).unwrap();
        objects.clear();
        Ok(())
    }

    async fn container_exists(
        &self,
        _account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<bool> {
        let containers = self.containers.read().unwrap();
        Ok(containers.contains_key(&container_name))
    }

    async fn copy_object(
        &self,
        _account_id: AccountId,
        source_container_name: String,
        source_object_name: String,
        destination_container_name: String,
        destination_object_name: String,
    ) -> anyhow::Result<()> {
        let mut containers = self.containers.write().unwrap();
        if !containers.contains_key(&source_container_name) {
            anyhow::bail!("Source container does not exist");
        }
        if !containers.contains_key(&destination_container_name) {
            anyhow::bail!("Destination container does not exist");
        }
        let source_container = &containers.get_mut(&source_container_name).unwrap().1;
        if !source_container.contains_key(&source_object_name) {
            anyhow::bail!("Source object does not exist");
        }
        let source_object = source_container.get(&source_object_name).unwrap().clone();
        let destination_container = &mut containers.get_mut(&destination_container_name).unwrap().1;
        destination_container.insert(destination_object_name, source_object.clone());
        Ok(())
    }

    async fn create_container(
        &self,
        _account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<u64> {
        let mut containers = self.containers.write().unwrap();
        if containers.contains_key(&container_name) {
            anyhow::bail!("Container already exists");
        }
        let created_at = chrono::Utc::now().timestamp_millis() as u64;
        containers.insert(container_name, (created_at, HashMap::new()));
        Ok(created_at)
    }

    async fn delete_container(
        &self,
        _account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<()> {
        let mut containers = self.containers.write().unwrap();
        if !containers.contains_key(&container_name) {
            anyhow::bail!("Container does not exist");
        }
        containers.remove(&container_name);
        Ok(())
    }

    async fn delete_object(
        &self,
        _account_id: AccountId,
        container_name: String,
        object_name: String,
    ) -> anyhow::Result<()> {
        let mut containers = self.containers.write().unwrap();
        if !containers.contains_key(&container_name) {
            anyhow::bail!("Container does not exist");
        }
        let (_, objects) = containers.get_mut(&container_name).unwrap();
        objects.remove(&object_name);
        Ok(())
    }

    async fn delete_objects(
        &self,
        _account_id: AccountId,
        container_name: String,
        object_names: Vec<String>,
    ) -> anyhow::Result<()> {
        let mut containers = self.containers.write().unwrap();
        if !containers.contains_key(&container_name) {
            anyhow::bail!("Container does not exist");
        }
        let (_, objects) = containers.get_mut(&container_name).unwrap();
        for object_name in object_names {
            objects.remove(&object_name);
        }
        Ok(())
    }

    async fn get_container(
        &self,
        _account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<Option<u64>> {
        let containers = self.containers.read().unwrap();
        if !containers.contains_key(&container_name) {
            return Ok(None);
        }
        let (created_at, _) = containers.get(&container_name).unwrap();
        Ok(Some(*created_at))
    }

    async fn get_data(
        &self,
        _account_id: AccountId,
        container_name: String,
        object_name: String,
        start: u64,
        end: u64,
    ) -> anyhow::Result<Vec<u8>> {
        let containers = self.containers.read().unwrap();
        if !containers.contains_key(&container_name) {
            anyhow::bail!("Container does not exist");
        }
        let (_, objects) = containers.get(&container_name).unwrap();
        if !objects.contains_key(&object_name) {
            anyhow::bail!("Object does not exist");
        }
        let (_, data) = objects.get(&object_name).unwrap();
        Ok(data[start as usize..end as usize].to_vec())
    }

    async fn has_object(
        &self,
        _account_id: AccountId,
        container_name: String,
        object_name: String,
    ) -> anyhow::Result<bool> {
        let containers = self.containers.read().unwrap();
        if !containers.contains_key(&container_name) {
            anyhow::bail!("Container does not exist");
        }
        let (_, objects) = containers.get(&container_name).unwrap();
        Ok(objects.contains_key(&object_name))
    }

    async fn list_objects(
        &self,
        _account_id: AccountId,
        container_name: String,
    ) -> anyhow::Result<Vec<String>> {
        let containers = self.containers.read().unwrap();
        if !containers.contains_key(&container_name) {
            anyhow::bail!("Container does not exist");
        }
        let (_, objects) = containers.get(&container_name).unwrap();
        Ok(objects.keys().cloned().collect())
    }

    async fn move_object(
        &self,
        _account_id: AccountId,
        source_container_name: String,
        source_object_name: String,
        destination_container_name: String,
        destination_object_name: String,
    ) -> anyhow::Result<()> {
        let mut containers = self.containers.write().unwrap();
        if !containers.contains_key(&source_container_name) {
            anyhow::bail!("Source container does not exist");
        }
        if !containers.contains_key(&destination_container_name) {
            anyhow::bail!("Destination container does not exist");
        }
        let source_container = &mut containers.get_mut(&source_container_name).unwrap().1;
        if !source_container.contains_key(&source_object_name) {
            anyhow::bail!("Source object does not exist");
        }
        let source_object = source_container.remove(&source_object_name).unwrap();
        let destination_container = &mut containers.get_mut(&destination_container_name).unwrap().1;
        destination_container.insert(destination_object_name, source_object);
        Ok(())
    }

    async fn object_info(
        &self,
        _account_id: AccountId,
        container_name: String,
        object_name: String,
    ) -> anyhow::Result<ObjectMetadata> {
        let containers = self.containers.read().unwrap();
        if !containers.contains_key(&container_name) {
            anyhow::bail!("Container does not exist");
        }
        let (_, objects) = containers.get(&container_name).unwrap();
        if !objects.contains_key(&object_name) {
            anyhow::bail!("Object does not exist");
        }
        let (created_at, data) = objects.get(&object_name).unwrap();
        Ok(ObjectMetadata {
            name: object_name,
            container: container_name,
            created_at: *created_at,
            size: data.len() as u64,
        })
    }

    async fn write_data(
        &self,
        _account_id: AccountId,
        container_name: String,
        object_name: String,
        data: Vec<u8>,
    ) -> anyhow::Result<()> {
        let mut containers = self.containers.write().unwrap();
        if !containers.contains_key(&container_name) {
            anyhow::bail!("Container does not exist");
        }
        let (_, objects) = containers.get_mut(&container_name).unwrap();
        objects.insert(
            object_name,
            (chrono::Utc::now().timestamp_millis() as u64, data),
        );
        Ok(())
    }
}

pub struct ObjectMetadata {
    pub name: String,
    pub container: String,
    pub created_at: u64,
    pub size: u64,
}
