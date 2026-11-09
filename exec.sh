#!/bin/bash
# Labdoo-Installer within zzzFM for UEFI and legacy devices
# Version 2.beta1 for Labtix 3 for Images with two or three
#partitions ( labdoo 22.04 , labdoo 24.04 )
# by thomsen for labdoo

$fm_import    # import file manager variables (scroll down for info)

image=${fm_filename}
hostname="labdoo-000xxxxx"
image_path=${fm_file}
shred="TRUE"
disk=sda
echo $image_path
root_path=$(echo $image_path | rev | cut -d "/" -f 3- | rev)
echo "root path:"
echo $root_path 
content_lists_path="$root_path/wiki-archive/content_list"
echo "content_list:"
echo $content_lists_path
echo "content_wikis_path"
content_wikis_path="$root_path/wiki-archive/wikis/"
echo $content_wikis_path
echo "content_kiwix_path:"
content_kiwix_path="$root_path/wiki-archive/kiwix/wikis/"
echo $content_kiwix_path

#read -p "01_image image_path, Enter !"

#exit



#partimag exisiting and or mounted ?
sudo test -e /home/partimag
if [ $? -eq 0 ]
then
yad --on-top --center --image=/home/labdoo/labtix/edu_tux_120.png --title="Labdoo-Installer" --info --button="OK" --buttons-layout=center --text-align=center --text="<b>\\n \\n  Sorry.\\n  \\n        A directory was mounted by another script at /home/partimag !!!   \\n \\n Unmount  /home/partimag with SpaceFM \\n  \\nand mount your SOURCE as a normal media !!! \\n \\n Never simply delete /home/partimage !!! \\n \\n This will destroy all your data in the mounted directory !!! \\n</b>" 2> /dev/null
exit
exit $?
fi

#clonezilla image ?
test -e $fm_file/clonezilla-img
if [ $? -eq 1 ]
then
yad --on-top --center --image=/home/labdoo/labtix/edu_tux_120.png --title="Labdoo-Installer" --info --button="OK" --buttons-layout=center --text-align=center --text="<b>\\n \\n \\n Sorry,\\n  \\n  \\n        this is no Clonezilla image folder !!!           </b>" 2> /dev/null
exit
fi

#===========================================================================

#possible target-disks

usable_disks=$(lsblk -d -n -o NAME)

boot_device=$(mount -l |grep /dev/s |head -1 |cut -d" " -f1|cut -c 6-8)

#echo $usable_disks

disks=$(echo $usable_disks)

echo $disks > /tmp/disks

#cat /tmp/disks

disks=$(sed 's/sc./ /g;s/sr./ /g;s/loop./ /g' /tmp/disks)

rm /tmp/disks
 
#define target-disk
disk="$(yad --on-top --center --image=/usr/share/pixmaps/edu-tux-icon.png --title="Labdoo-Installer" --entry --entry-text="sda" --button="OK" --text="  \\n \\n Possible <b>TARGET-DISKS:</b> \\n \\n <b>$disks</b> \\n \\n Do not use your <b>SOURCE-DISK</b> !  \\n \\n BOOTDEVICE :       <b> $boot_device </b>\\n " 2> /dev/null)"

echo $disk

#sudo test -e /dev/{$disk}3
#if [ $? -eq 0 ]
#then
#partition=sda3
#else
#partition=sda2
#exit $?
#fi
#print $partition
#read -p "aaaaa disk Enter"
##exit

#target = boot-device ?
boot_device=$(cat /live/config/boot-device |cut -c 6-8)
if [ $boot_device == $disk ]
then
yad --on-top --center --image=/home/labdoo/labtix/edu_tux_120.png --title="Labdoo-Installer" --info --button="OK" --buttons-layout=center --text-align=center --text="\\n \\n \\n Bad choice,\\n            $disk is your boot-device !           " 2> /dev/null
exit
fi

#homefolder-cleaning
sudo rm -f /home/labdoo/wikis*
sudo rm -f /home/labdoo/kiwix*
sudo rm -f /home/labdoo/lang*
sudo rm -f /home/labdoo/homepages*
sudo rm /home/labdoo/cont_missed
sudo rm /home/labdoo/cont_installed
sudo rm -f /home/labdoo/info

# form with shred times
yad \
--center \
--on-top \
--info \
--title="Labdoo-Installer" \
--text="\\n     Image <b>$image</b> \\n     will be cloned to <b>$disk</b>  !!!  \\n " \
--image="/usr/share/pixmaps/edu-tux-icon.png" \
--form \
--field="Shred $disk: \\n3=secure 0=no ":NUM 3!0..3!1 \
--field="Hostname  :TEXT" \
--field=" Contents for Basic-Image :FL" \
--field="   Poweroff After Installation ?:CHK" \
"labdoo-000xxxxx" "$content_lists_path/none" "FALSE" > /home/labdoo/labtix/yadtest 


out1=$?

echo $out1

cat /home/labdoo/labtix/yadtest

#--field="Poweroff ?: \\n0=no 1=yes 2=sleep ":NUM 0!0..2!1 \#

#read -p "02_form output, Enter !"

if [ $out1 == 1 ]
then
echo "Exit"
exit
fi

# variables

shred=$(cat /home/labdoo/labtix/yadtest |cut '-d|' -f1)
#echo "shred :$shred"
hostname=$(cat /home/labdoo/labtix/yadtest |cut '-d|' -f2)
#echo "hostname: $hostname"
poweroff=$(cat /home/labdoo/labtix/yadtest |cut '-d|' -f4)
#echo "poweroff : $poweroff"

cont_list=$(cat /home/labdoo/labtix/yadtest |cut '-d|' -f3)

# ================= info section

sed -n "/####info/,/^$/p" $cont_list > /home/labdoo/info
sed -i "1d" /home/labdoo/info
info=$(cat /home/labdoo/info )

#cat /home/labdoo/info

if [ -n "$info" ]; then
	yad --on-top --center --height=280 --width=550 --image=/home/labdoo/labtix/edu_tux_120.png --title="Labdoo-Installer" --info --button="OK" --buttons-layout=center --text "\n Additional Information: \n" --text-align=center --text-info < /home/labdoo/info 2> /dev/null
fi

sudo rm -f /home/labdoo/info
#read -p "03_info"

#====================================



#killing existing /home/partimag , make a new /home/partimag and link the selected image-directory to it
sudo umount /tmp/local-dev
sudo umount /home/partimag
sudo rm -r /home/partimag
sudo mkdir /home/partimag
sudo ln -s $image_path /home/partimag


# shred  

sudo shred /dev/$disk -v -f -n $shred

#========================================================================
#Clonezilla_Startup:

sudo /usr/sbin/ocs-sr -e1 auto -e2 -r -icrc -icds -scr -j2 -k1 -p true restoredisk $image $disk


#=======================================================================
#### define target-partition for 
#echo $disk
##read -p "disk"
##check if target is nvme or mmcblk device
#part_number=3

#if [ $disk = nvme0n1 ]
	#then
	#part="p$part_number"
#elif [ $disk = mmcblk0 ]
	#then
	#part="p$part_number"
#else
	#part="$part_number"
#fi

##define labdoo main partition
#disk1="$disk$part"
#sleep 2
#echo $disk1
##sudo resize2fs /dev/$disk1
#echo "disk1"
##sudo gparted

## -p "Enter !"

##exit

echo $disk

#### creating partnumber for spart
if [ $disk = nvme0n1 ]
	then
	spart="p3"
elif [ $disk = mmcblk0 ]
	then
	spart="p3"
else
	spart="3"
fi

echo $spart
#read -p "00 spart"

#### search for partition 3
if test -e /dev/$disk$spart ; then
	echo "File exists."
	part_number=3
else
	echo "no such file"
	part_number=2
fi

echo $part_number
#read -p "01    part_number Enter"
#exit

#disk=nvme0n1

#part_number=3

#### create partnumber for different devices
if [ $disk = nvme0n1 ]
	then
	part="p$part_number"
elif [ $disk = mmcblk0 ]
	then
	part="p$part_number"
else
	part="$part_number"
fi

echo $part
#read -p "02    part"

#define labdoo main partition
disk1="$disk$part"
sleep 2
echo $disk1
#sudo resize2fs /dev/$disk1
echo "03   disk1"


#=======================================================================
#Change hostid
sudo mount /dev/$disk1 /mnt

if [ $hostname = labdoo-000xxxxx ]; then
	echo "Not Setting new hostname"
else
	echo "Setting new hostid ${hostname} as requested"
	sudo sed -i "s/labdoo........./${hostname}/g" /mnt/etc/hosts
        sudo sed -i "s/labdoo........./${hostname}/g" /mnt/etc/hostname
fi;


#=======================================================================
#=================install contents =====================================
#==define arrays and variable



loops=$(grep -c "####lang_" $cont_list)

echo $loops
#loops=4

##read -p "Loops"

for ((counter=1 ; counter<=loops ; counter++ )); do
echo $counter
#read -p "04_Enter"
count=$counter

sed -n "/####lang_$count/,/^$/p" $cont_list > /home/labdoo/lang_1
sed -i "1d" /home/labdoo/lang_1
lang_1=$(cat /home/labdoo/lang_1 )

echo
echo "lang_1 :"
echo $lang_1

sed -n "/####homepages_$count/,/^$/p" $cont_list > /home/labdoo/homepages_1
sed -i "1d" /home/labdoo/homepages_1
homepages_1=$(cat /home/labdoo/homepages_1 )


echo
echo $homepages_1

#read -p "05_langs and homepages Enter"


sed -n "/####wikis_$count/,/^$/p" $cont_list > /home/labdoo/wikis_1
sed -i "1d" /home/labdoo/wikis_1
mapfile -t wikis_1_ar < /home/labdoo/wikis_1
echo
echo "wikis_1 :"
	for i in ${wikis_1_ar[@]}
	do
		echo $i
	done



sed -n "/####kiwix_$count/,/^$/p" $cont_list > /home/labdoo/kiwix_1
sed -i "1d" /home/labdoo/kiwix_1
mapfile -t kiwix_1_ar < /home/labdoo/kiwix_1
echo
echo "kiwix_1 :"
	for i in ${kiwix_1_ar[@]}
	do
		echo $i
	done




#read -p "06_enter"

##### install contents ##############################################

#unpack wiki content
for i in ${wikis_1_ar[@]}
do
	echo "==========unpack"
	echo $i
	if [ ! -f "$content_wikis_path/$lang_1/$i" ]; then
		echo "$i" >> /home/labdoo/cont_missed
		else
		echo $i >> /home/labdoo/cont_installed
	fi
	sudo tar --use-compress-program="pigz -d" -xvf $content_wikis_path/$lang_1/$i -C /mnt/home/labdoo/Public/wikis/$lang_1
done


####set firefox homepages prefs
labdoo_folder=$(sudo find /mnt/home/labdoo/.mozilla/firefox-esr -type d -name "*default-esr*")
student_folder=$(sudo find /mnt/home/student/.mozilla/firefox-esr -type d -name "*default-esr*")

#echo "labdoo-esr-profile: $labdoo_folder"
#homepages=$(sudo cat $labdoo_folder/prefs.js | grep 'user_pref("browser.startup.homepage",' | cut -c 1-39 --complement | rev | cut -c 1-3 --complement | rev )
#echo "homepages: $homepages"
echo "$homepages_1" > /home/labdoo/homepages

cat /home/labdoo/homepages

#read -p "07_2nd_homepages"

new_homepages=$(cat /home/labdoo/homepages)
#echo $new_homepages
#echo "user_pref('browser.startup.homepage', '$new_homepages');" > /home/labdoo/user.js
#read -p " Enter "
student_folder=$(sudo find /mnt/home/student/.mozilla/firefox-esr -type d -name "*default-esr*")
#echo "student-esr-profile: $student_folder"

#sudo cat $student_folder/prefs.js | grep 'user_pref("browser.startup.homepage",' | cut -c 1-39 --complement | rev | cut -c 1-3 --complement | rev
####  s# sets sepatator to "#" , "\" masks quotes
sudo sed -i -e "s#user_pref(\"browser.startup.homepage\",.*#user_pref(\"browser.startup.homepage\", \"$new_homepages\");#" $labdoo_folder/prefs.js
sudo sed -i -e "s#user_pref(\"browser.startup.homepage\",.*#user_pref(\"browser.startup.homepage\", \"$new_homepages\");#" $student_folder/prefs.js



#====================
#copying kiwix wikis

for i in ${kiwix_1_ar[@]}
do
	echo "============copy"
	echo $i
	if [ ! -f $content_kiwix_path/$lang_1/$i ]; then
		echo $i >> /home/labdoo/cont_missed
		else
		echo $i >> /home/labdoo/cont_installed
	fi
	sudo rsync --progress $content_kiwix_path/$lang_1/$i /mnt/home/labdoo/Public/kiwix/wikis
done

sudo rm /home/labdoo/wikis*
sudo rm /home/labdoo/kiwix*
sudo rm /home/labdoo/lang*
sudo rm /home/labdoo/homepages*
done



#read -p "08_Enter "

#=======================================================================
#### set permissions to Public folder

# Stefan Abeln 27.8.2015, modified 01.07.2016 , modified by thomsen 07.10.2023
# Set sticky bit on /home/labdoo/Public so that all newly created files or directories will be owned by user and group labdoo and are accessible by labdoo and members of group labdoo
echo "Ensuring that newly created files and directories will be owned by labdoo and belong to group labdoo"
sudo chmod ug+s /mnt/home/labdoo/Public
# Set ownership of newly created files and directories to labdoo:labdoo
echo "Correcting possible false ownerships"
sudo chown --recursive labdoo:labdoo /mnt/home/labdoo/Public
# Make sure that xowa can be started by student even if he's the first to start xowa
# therefore make sure all files and directories belong to group labdoo and are writable for the group
sudo chmod --recursive a+rw,o-w /mnt/home/labdoo/Public/kiwix
# Make /Public also writable to student
sudo chmod --recursive a+rw,o-w /mnt/home/labdoo/Public 
# Change to the right permissions for directories and files
# Make "how-to-start" only writable to labdoo
sudo chmod --recursive a+rw,g-w,o-w /mnt/home/labdoo/Public/how-to-start 
# Make "manuals" only writable to labdoo
sudo chmod --recursive a+rw,g-w,o-w /mnt/home/labdoo/Public/manuals 

# Set rights for folders in /Public right
echo "Ensure that members of group labdoo can enter directories"
sudo find /mnt/home/labdoo/Public -type d -exec chmod u=rwx,g=rwx,o=rx {} \;

#read -p "check mounting point"
sudo rm /mnt/etc/machine-id
sudo rm /mnt/var/lib/dbus/machine-id
sudo dbus-uuidgen | sudo tee /mnt/etc/machine-id

#sudo dbus-uuidgen | sudo tee /mnt/etc/machine-id

#read -p "guckst du /mnt/var/lib/dbus/machine-id und /mnt/etc/machine-id dann ENTERN"



#read -p " 09_permissions set "
echo "============================================="
echo "installed content:"
sudo ls -a /mnt/home/labdoo/Public/kiwix/wikis
sudo ls -a /mnt/home/labdoo/Public/wikis/EN
echo "============================================="
echo "/etc/machine-id:"
sudo cat /mnt/etc/machine-id
sudo cat /mnt/var/lib/dbus/machine-id
echo "============================================="

#read -p "check machine-id"
#=======================================================================
#=====================cleanup
sudo umount /mnt

sudo umount /tmp/local-dev
sudo umount /home/partimag
sudo rm -r /home/partimag

#=======================
# show missing contents

missed=$(cat /home/labdoo/cont_missed)

if [ -n "$missed" ]; then
	yad --on-top --center --width=400 --image=/home/labdoo/labtix/edu_tux_120.png --title="Labdoo-Installer" --info --button="OK" --buttons-layout=center --text "\n These content-files do not exist: \n" --text-align=center --text-info < /home/labdoo/cont_missed 2> /dev/null
fi

sudo rm /home/labdoo/cont_missed

#=======================================================================
#poweroff ?
if [ $poweroff = TRUE ]; then
	echo "Poweroff the device"
	sleep 5
	sudo poweroff
else
echo " "
	sudo gparted /dev/$disk
fi



#=======================================================================
#finished !!!
yad --on-top --height=400 --center --image=/home/labdoo/labtix/edu_tux_120.png --title="Labdoo-Installer" --info --button="OK" --buttons-layout=center --text-align=center --text="\\n   Finished !!! \\n \\n You've cloned $image to $disk . \\n \\n Installed Content : " --text-align=center --text-info < /home/labdoo/cont_installed 2> /dev/null

sudo rm /home/labdoo/cont_installed
sudo rm /home/labdoo/loops


exit    

exit $?



